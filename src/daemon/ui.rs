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
    deck.ui_command_tx.send(UiCommand::PopPage).await?;
    Ok(())
}

async fn btn_push(deck: &mut NoiseDeck, id: Uuid) -> eyre::Result<()> {
    let buttons = deck.get_library_category(&id)?;
    deck.push_page(buttons).await?;
    Ok(())
}

async fn btn_play(deck: &mut NoiseDeck, track: &Arc<Track>) -> eyre::Result<()> {
    deck.audio_command_tx
        .send(AudioCommand::Play(track.clone()))
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct ButtonData {
    pub label: Arc<String>,
}

pub struct NoiseDeck {
    ui_command_tx: Sender<UiCommand>,
    ui_event_rx: Receiver<UiEvent>,
    audio_command_tx: Sender<AudioCommand>,
    audio_event_rx: Receiver<AudioEvent>,

    kind: Kind,
    config: Arc<Config>,
    library: HashMap<Uuid, LibraryCategoryState>,
}

struct LibraryCategoryState {
    id: Uuid,
    config: Arc<config::Page>,
    buttons: Vec<ButtonRef>,
}

impl NoiseDeck {
    pub(crate) async fn push_page(&mut self, buttons: Vec<Option<ButtonRef>>) -> eyre::Result<()> {
        self.ui_command_tx
            .send(UiCommand::PushPage(buttons))
            .await?;
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
            config,
            library: HashMap::new(),
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
        let rendered_buttons = self.get_library_category(&self.config.start_page.clone())?;
        self.push_page(rendered_buttons).await?;
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_library_category(&mut self, page_id: &Uuid) -> eyre::Result<Vec<Option<ButtonRef>>> {
        fn layout_library_category(page: &config::Page, kind: &Kind) -> eyre::Result<Vec<ButtonRef>> {
            let max_configured_buttons = kind.key_count() as usize - 1;
            let padding_btn_cnt =
                max_configured_buttons - page.buttons.len().min(max_configured_buttons);
            debug!("Padding buttons: {}", padding_btn_cnt);
            let rendered_buttons = page
                .buttons
                .iter()
                .take(max_configured_buttons)
                .map(|b| match &b.behavior {
                    config::ButtonBehavior::PushPage(id) => Button::builder()
                        .data(ButtonData {
                            label: b.label.clone(),
                        })
                        .on_tap(ButtonBehavior::Push(id.clone()))
                        .build()
                        .into(),
                    config::ButtonBehavior::PlaySound { path } => Button::builder()
                        .data(ButtonData {
                            label: b.label.clone(),
                        })
                        .on_tap(ButtonBehavior::Play)
                        .track(Arc::new(PathBuf::from(&path[..])))
                        .build()
                        .into(),
                })
                .chain([Button::builder()
                    .data(ButtonData {
                        label: "Back".to_string().into(),
                    })
                    .on_tap(ButtonBehavior::Pop)
                    .build()
                    .into()])
                .chain(repeat(Button::none()).take(padding_btn_cnt))
                .collect();
            Ok(rendered_buttons)
        }

        let state = match self.library.entry(page_id.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let page = self
                    .config
                    .pages
                    .get(&page_id)
                    .expect("page not found")
                    .clone();
                let initial_state = LibraryCategoryState {
                    id: page_id.clone(),
                    buttons: layout_library_category(&page, &self.kind)?,
                    config: page,
                };
                &*e.insert(initial_state)
            }
        };

        Ok(state.buttons.iter().map(|b| Some(b.clone())).collect())
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
                }
            }
        }
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
