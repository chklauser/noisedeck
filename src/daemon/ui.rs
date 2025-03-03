use crate::config;
use crate::config::Config;
use crate::daemon::audio::{AudioCommand, AudioEvent, Track};
use elgato_streamdeck::info::Kind;
use eyre::OptionExt;
use std::iter::repeat;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Default)]
pub struct Button {
    data: tokio::sync::RwLock<ButtonData>,
    pub track: Option<Arc<Track>>,
    on_tap: Option<ButtonBehavior>,
}
impl Button {
    pub(crate) fn builder() -> ButtonBuilder {
        ButtonBuilder {
            inner: Button::default(),
        }
    }

    pub fn none() -> ButtonRef {
        static NONE: LazyLock<ButtonRef> = LazyLock::new(|| Button::builder().build().into());
        NONE.clone()
    }
}
pub struct ButtonBuilder {
    inner: Button,
}

pub enum ButtonBehavior {
    Push(Uuid),
    Play,
    Pop,
}
impl ButtonBehavior {
    pub async fn invoke(
        &self,
        deck: &mut NoiseDeck,
        button: &Button,
        _data: &mut ButtonData,
    ) -> eyre::Result<()> {
        match self {
            ButtonBehavior::Pop => {
                ButtonBehavior::pop(deck).await?;
            }
            ButtonBehavior::Push(id) => {
                ButtonBehavior::push(deck, id.clone()).await?;
            }
            ButtonBehavior::Play => {
                if let Some(track) = &button.track {
                    ButtonBehavior::play(deck, track).await?;
                } else {
                    warn!("Button has no track assigned");
                }
            }
        }
        Ok(())
    }

    async fn pop(deck: &mut NoiseDeck) -> eyre::Result<()> {
        deck.ui_command_tx.send(UiCommand::PopPage).await?;
        Ok(())
    }

    async fn push(deck: &mut NoiseDeck, id: Uuid) -> eyre::Result<()> {
        let buttons = deck.render_page(&id)?;
        deck.push_page(buttons).await?;
        Ok(())
    }

    async fn play(deck: &mut NoiseDeck, track: &Arc<Track>) -> eyre::Result<()> {
        deck.audio_command_tx
            .send(AudioCommand::Play(track.clone()))
            .await?;
        Ok(())
    }
}

impl ButtonBuilder {
    pub fn on_tap(mut self, behavior: ButtonBehavior) -> Self {
        self.inner.on_tap = Some(behavior);
        self
    }

    pub fn data(mut self, data: ButtonData) -> Self {
        *self.inner.data.get_mut() = data;
        self
    }

    pub fn track(mut self, track_path: Arc<PathBuf>) -> Self {
        self.inner.track = Some(Arc::new(Track::new(track_path)));
        self
    }

    pub fn build(self) -> Button {
        self.inner
    }
}

impl std::fmt::Debug for Button {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Button").field("data", &self.data).finish()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct ButtonData {
    pub label: Arc<String>,
}

#[derive(Clone)]
pub struct ButtonRef {
    inner: Arc<Button>,
}
impl ButtonRef {
    pub async fn read(&self) -> ButtonData {
        self.inner.data.read().await.clone()
    }
}
impl From<Button> for ButtonRef {
    fn from(inner: Button) -> Self {
        ButtonRef {
            inner: Arc::new(inner),
        }
    }
}
impl std::fmt::Debug for ButtonRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ButtonRef")
            .field("data", &self.inner.data)
            .finish()
    }
}

impl PartialEq for ButtonRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}
impl Eq for ButtonRef {}

pub struct NoiseDeck {
    ui_command_tx: Sender<UiCommand>,
    ui_event_rx: Receiver<UiEvent>,
    audio_command_tx: Sender<AudioCommand>,
    audio_event_rx: Receiver<AudioEvent>,

    kind: Kind,
    config: Arc<Config>,
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
        let rendered_buttons = self.render_page(&self.config.start_page.clone())?;
        self.push_page(rendered_buttons).await?;
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn render_page(&mut self, page_id: &Uuid) -> eyre::Result<Vec<Option<ButtonRef>>> {
        let page = self
            .config
            .pages
            .get(&page_id)
            .ok_or_eyre("Start page not found")?;

        let max_configured_buttons = self.kind.key_count() as usize - 1;
        let padding_btn_cnt = max_configured_buttons - page.buttons.len().min(max_configured_buttons);
        debug!("Padding buttons: {}", padding_btn_cnt);
        let rendered_buttons = page
            .buttons
            .iter()
            .take(max_configured_buttons)
            .map(|b| {
                Some(match &b.behavior {
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
            })
            .chain([Some(
                Button::builder()
                    .data(ButtonData {
                        label: "Back".to_string().into(),
                    })
                    .on_tap(ButtonBehavior::Pop)
                    .build()
                    .into(),
            )])
            .chain(
                repeat(Some(Button::none()))
                    .take(padding_btn_cnt),
            )
            .collect();
        Ok(rendered_buttons)
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

#[derive(Debug)]
pub enum UiEvent {
    ButtonTap(ButtonRef),
}

pub enum UiCommand {
    Refresh,
    PushPage(Vec<Option<ButtonRef>>),
    PopPage,
}

impl std::fmt::Debug for UiCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiCommand::Refresh => f.write_str("Refresh"),
            UiCommand::PushPage(_) => f.write_str("PushPage"),
            UiCommand::PopPage => f.write_str("PopPage"),
        }
    }
}
