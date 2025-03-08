use crate::config;
use crate::config::Config;
use crate::daemon::audio::{AudioCommand, AudioEvent, Track};
use crate::daemon::ui::btn::{Button, ButtonBehavior};
use elgato_streamdeck::info::Kind;
use eyre::OptionExt;
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
    if deck.nav_stack.len() <= 1 {
        debug!("ignoring pop at home page");
        return Ok(());
    }

    deck.nav_stack.pop();
    deck.display_top_page().await
}

async fn btn_push(deck: &mut NoiseDeck, id: Uuid) -> eyre::Result<()> {
    deck.nav_stack.push(id);
    deck.display_top_page().await
}

async fn btn_play_stop(deck: &mut NoiseDeck, track: &Arc<Track>) -> eyre::Result<()> {
    let state = track.read().await;
    let track = track.clone();
    deck.audio_command_tx
        .send(if state.is_playing {
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
    config: Arc<Config>,
    library: HashMap<Uuid, LibraryCategoryState>,
    tracks: HashMap<Arc<PathBuf>, ButtonRef>,
    nav_stack: Vec<Uuid>,
}

struct LibraryCategoryState {
    id: Uuid,
    config: Arc<config::Page>,
    buttons: Vec<ButtonRef>,
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
            kind,
            nav_stack: vec![config.start_page],
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
        btn_push(self, self.config.start_page).await
    }

    fn layout_page(&self, semantic_buttons: &[ButtonRef]) -> Vec<Option<ButtonRef>> {
        let mut page = Vec::with_capacity(self.kind.key_count().into());
        let (n_rows, n_cols) = self.kind.key_layout();
        let n_content_rows = n_rows - 1;
        page.extend(
            semantic_buttons
                .iter()
                .take((n_content_rows * n_cols).into())
                .map(|b| Some(b.clone())),
        );
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
        page.extend(repeat(None).take((n_cols - 1).into()));
        page
    }

    async fn display_top_page(&mut self) -> eyre::Result<()> {
        let page_id = *self.nav_stack.last().ok_or_eyre("nav stack empty")?;
        let semantic_buttons = self.get_library_category(&page_id)?;
        let physical_buttons = self.layout_page(&semantic_buttons);
        self.ui_command_tx
            .send(UiCommand::Flip(physical_buttons))
            .await?;
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_library_category(&mut self, page_id: &Uuid) -> eyre::Result<Vec<ButtonRef>> {
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

        Ok(state.buttons.clone())
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
        btn_state.notification = if track_state.is_playing {
            if let Some(remaining) = track_state.rem_duration() {
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
