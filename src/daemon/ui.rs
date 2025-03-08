use crate::config;
use crate::config::Config;
use crate::daemon::audio::{AudioCommand, AudioEvent, Track};
use crate::daemon::ui::btn::{Button, ButtonBehavior};
use elgato_streamdeck::info::Kind;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::iter::repeat;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, info, warn};
use uuid::Uuid;

mod btn;

pub use btn::ButtonRef;

async fn btn_pop(deck: &mut NoiseDeck) -> eyre::Result<()> {
    if deck.view_stack.len() <= 1 {
        debug!("ignoring pop at home page");
        return Ok(());
    }

    deck.view_stack.pop();
    deck.display_top_page().await
}

async fn btn_push(deck: &mut NoiseDeck, id: Uuid) -> eyre::Result<()> {
    deck.view_stack.push(View::new(id));
    deck.display_top_page().await
}

async fn btn_rotate(deck: &mut NoiseDeck) -> eyre::Result<()> {
    let geo = deck.geo;
    let view = deck.current_view()?;
    let page_len = deck.get_library_category(&view.page_id.clone())?.len();
    let view = deck.current_view_mut()?;
    view.offset += geo.n_content;
    if view.offset >= page_len {
        view.offset = 0;
    }
    deck.display_top_page().await
}

async fn btn_play_stop(deck: &mut NoiseDeck, track: &Arc<Track>) -> eyre::Result<()> {
    let state = track.read().await;
    let track = track.clone();
    deck.audio_command_tx
        .send(if state.playback.is_advancing() {
            AudioCommand::Stop(track)
        } else {
            AudioCommand::Play(track)
        })
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct ButtonData {
    pub label: Arc<String>,
    pub notification: Option<String>,
}

pub struct NoiseDeck {
    ui_command_tx: Sender<UiCommand>,
    ui_event_rx: Receiver<UiEvent>,
    audio_command_tx: Sender<AudioCommand>,
    audio_event_rx: Receiver<AudioEvent>,

    kind: Kind,
    geo: Geometry,
    config: Arc<Config>,
    library: HashMap<Uuid, LibraryCategoryState>,
    tracks: HashMap<Arc<PathBuf>, ButtonRef>,
    view_stack: Vec<View>,
}

#[derive(Debug)]
pub struct View {
    page_id: Uuid,
    offset: usize,
}
impl View {
    pub fn new(page_id: Uuid) -> Self {
        View { page_id, offset: 0 }
    }
}

struct LibraryCategoryState {
    id: Uuid,
    config: Arc<config::Page>,
    buttons: Vec<ButtonRef>,
}

#[derive(Debug, Copy, Clone)]
struct Geometry {
    cols: usize,
    rows: usize,
    n_content: usize,
    n_dynamic: usize,
}
impl From<Kind> for Geometry {
    fn from(kind: Kind) -> Self {
        let (rows, cols) = kind.key_layout();
        let n_content = (rows - 1) * cols;
        let n_dynamic = cols - 2;
        Geometry {
            cols: cols.into(),
            rows: rows.into(),
            n_content: n_content.into(),
            n_dynamic: n_dynamic.into(),
        }
    }
}

impl NoiseDeck {
    pub(crate) async fn push_page(&mut self, buttons: Vec<Option<ButtonRef>>) -> eyre::Result<()> {
        self.ui_command_tx.send(UiCommand::Flip(buttons)).await?;
        Ok(())
    }

    pub fn new(
        kind: Kind,
        config: Arc<Config>,
    ) -> (
        Self,
        Sender<UiEvent>,
        Receiver<UiCommand>,
        Sender<AudioEvent>,
        Receiver<AudioCommand>,
    ) {
        let (audio_event_tx, audio_event_rx) = tokio::sync::mpsc::channel(16);
        let (audio_command_tx, audio_command_rx) = tokio::sync::mpsc::channel(16);
        let (ui_event_tx, ui_event_rx) = tokio::sync::mpsc::channel(16);
        let (ui_command_tx, ui_command_rx) = tokio::sync::mpsc::channel(16);
        let deck = NoiseDeck {
            ui_command_tx,
            ui_event_rx,
            audio_command_tx,
            audio_event_rx,
            geo: kind.into(),
            kind,
            view_stack: vec![View::new(config.start_page)],
            config,
            library: HashMap::new(),
            tracks: HashMap::new(),
        };
        (
            deck,
            ui_event_tx,
            ui_command_rx,
            audio_event_tx,
            audio_command_rx,
        )
    }

    pub async fn init(&mut self) -> eyre::Result<()> {
        self.display_top_page().await
    }

    fn layout_page(&self, semantic_buttons: &[ButtonRef], view: &View) -> Vec<Option<ButtonRef>> {
        let mut page = Vec::with_capacity(self.kind.key_count().into());

        // Content
        page.extend(
            semantic_buttons
                .iter()
                .skip(view.offset)
                .take(self.geo.n_content)
                .map(|b| Some(b.clone())),
        );

        // Pad content section
        page.extend(repeat(None).take(self.geo.n_content - page.len()));

        // Back
        page.push(Some(
            Button::builder()
                .data(ButtonData {
                    label: "Back".to_string().into(),
                    ..Default::default()
                })
                .on_tap(ButtonBehavior::Pop)
                .build()
                .into(),
        ));

        // Dynamic
        page.extend(repeat(None).take(self.geo.n_dynamic));

        // Next
        let total_n_pages = semantic_buttons.len() / self.geo.n_content
            + (if semantic_buttons.len() % self.geo.n_content > 0 {
                1
            } else {
                0
            });
        let current_page = view.offset / self.geo.n_content + 1;
        page.push(Some(
            Button::builder()
                .data(ButtonData {
                    label: format!("Next\n{current_page}/{total_n_pages}").into(),
                    ..Default::default()
                })
                .on_tap(ButtonBehavior::Rotate)
                .build()
                .into(),
        ));

        debug_assert_eq!(page.len(), self.kind.key_count() as usize);
        page
    }

    #[inline]
    fn current_view(&self) -> eyre::Result<&View> {
        self.view_stack
            .last()
            .ok_or_else(|| eyre::eyre!("nav stack empty"))
    }

    #[inline]
    fn current_view_mut(&mut self) -> eyre::Result<&mut View> {
        self.view_stack
            .last_mut()
            .ok_or_else(|| eyre::eyre!("nav stack empty"))
    }

    async fn display_top_page(&mut self) -> eyre::Result<()> {
        let semantic_buttons = self
            .get_library_category(&self.current_view()?.page_id.clone())?
            .to_vec();
        let physical_buttons = self.layout_page(&semantic_buttons, self.current_view()?);
        self.ui_command_tx
            .send(UiCommand::Flip(physical_buttons))
            .await?;
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_library_category(&mut self, page_id: &Uuid) -> eyre::Result<&[ButtonRef]> {
        fn layout_library_category(
            page: &config::Page,
            kind: &Kind,
        ) -> eyre::Result<Vec<ButtonRef>> {
            let max_configured_buttons = kind.key_count() as usize - 1;
            let padding_btn_cnt =
                max_configured_buttons - page.buttons.len().min(max_configured_buttons);
            debug!("Padding buttons: {}", padding_btn_cnt);
            let track_buttons = page
                .buttons
                .iter()
                .take(max_configured_buttons)
                .map(|b| match &b.behavior {
                    config::ButtonBehavior::PushPage(id) => Button::builder()
                        .data(ButtonData {
                            label: b.label.clone(),
                            ..Default::default()
                        })
                        .on_tap(ButtonBehavior::Push(*id))
                        .build()
                        .into(),
                    config::ButtonBehavior::PlaySound { path } => Button::builder()
                        .data(ButtonData {
                            label: b.label.clone(),
                            ..Default::default()
                        })
                        .on_tap(ButtonBehavior::PlayStop)
                        .track(Arc::new(PathBuf::from(&path[..])))
                        .build()
                        .into(),
                })
                .collect();
            Ok(track_buttons)
        }

        let state =
            match self.library.entry(*page_id) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let page = self
                        .config
                        .pages
                        .get(page_id)
                        .expect("page not found")
                        .clone();
                    let buttons = layout_library_category(&page, &self.kind)?;
                    self.tracks.extend(buttons.iter().filter_map(|b| {
                        b.inner.track.as_ref().map(|t| (t.path.clone(), b.clone()))
                    }));
                    let initial_state = LibraryCategoryState {
                        id: *page_id,
                        buttons,
                        config: page,
                    };
                    &*e.insert(initial_state)
                }
            };

        Ok(&state.buttons)
    }

    #[tracing::instrument(skip_all)]
    pub async fn run(mut self) -> eyre::Result<()> {
        loop {
            tokio::select! {
                event = self.ui_event_rx.recv() => {
                    match event {
                        Some(UiEvent::ButtonTap(button)) => {
                            if let Err(e) = self.handle_button_tap(&button).await {
                                warn!(error = %e, "Error handling button tap event");
                            }
                        }
                        None => {
                            info!("Event channel closed, shutting down");
                            break;
                        }
                    }
                },
                event = self.audio_event_rx.recv() => {
                    match event {
                        Some(AudioEvent::TrackStateChanged(track)) => {
                            if let Err(e) = self.handle_track_state_changed(track).await {
                                warn!(error = %e, "Error handling button tap event");
                            }
                        }
                        None => {
                            info!("Audio channel closed. I sure hope this is part of a shutdown sequence");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "trace")]
    async fn handle_track_state_changed(&mut self, track: Arc<Track>) -> eyre::Result<()> {
        let Some(btn) = self.tracks.get(&track.path) else {
            warn!("Track state changed for unknown track {:?}", track);
            return Ok(());
        };
        let mut btn_state = btn.inner.data.write().await;
        let track_state = track.read().await;
        btn_state.notification = if track_state.playback.is_advancing() {
            if let Some(remaining) = track_state.rem_duration {
                Some(format!("▶️\n{:.1}s", remaining.as_secs_f64()))
            } else {
                Some("▶️".to_string())
            }
        } else {
            None
        };
        drop(btn_state);
        self.ui_command_tx.send(UiCommand::Refresh).await?;
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "trace")]
    async fn handle_button_tap(&mut self, button: &ButtonRef) -> eyre::Result<()> {
        if let Some(on_tap) = button.inner.on_tap.as_ref() {
            {
                let mut button_guard = button.inner.data.write().await;
                on_tap
                    .invoke(self, &button.inner, &mut button_guard)
                    .await?;
            }
            self.ui_command_tx.send(UiCommand::Refresh).await?;
        } else {
            debug!("Button tap event received, but no handler set");
        }
        Ok(())
    }
}

mod iface;
pub use iface::{UiCommand, UiEvent};
