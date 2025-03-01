use elgato_streamdeck::info::Kind;
use std::iter::repeat;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, info, warn};

#[derive(Default)]
pub struct Button {
    data: tokio::sync::RwLock<ButtonData>,
    on_tap:  Option<ButtonBehavior>,
}
impl Button {
    pub(crate) fn builder() -> ButtonBuilder {
        ButtonBuilder {
            inner: Button::default(),
        }
    }
}
pub struct ButtonBuilder {
    inner: Button,
}

pub enum ButtonBehavior {
    Increment(u8),
}
impl ButtonBehavior {
    pub async fn invoke(&self, deck: &mut NoiseDeck, data: &mut ButtonData) -> eyre::Result<()> {
        match self {
            ButtonBehavior::Increment(i) => {
                ButtonBehavior::increment(*i, deck, data)?;
            }
        }
        Ok(())
    }
    
    fn increment(i: u8, deck: &mut NoiseDeck, data: &mut ButtonData) -> eyre::Result<()> {
        let state = &mut deck.state[i as usize];
        *state += 1;
        data.label = format!("Btn {i}\n{state}").into();
        Ok(())
    }
}

impl ButtonBuilder {
    pub fn on_tap(mut self, behavior: ButtonBehavior) -> Self
    {
        self.inner.on_tap = Some(behavior);
        self
    }
    
    pub fn data(mut self, data: ButtonData) -> Self {
        *self.inner.data.get_mut() = data;
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
    command_tx: Sender<Command>,
    event_rx: Receiver<Event>,

    pub state: Vec<u16>,
}

impl NoiseDeck {
    pub(crate) async fn push_page(&mut self, buttons: Vec<Option<ButtonRef>>) -> eyre::Result<()> {
        self.command_tx.send(Command::PushPage(buttons)).await?;
        Ok(())
    }
}

impl NoiseDeck {
    pub fn new(kind: Kind) -> (Self, Sender<Event>, Receiver<Command>) {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(16);
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(16);
        let deck = NoiseDeck {
            command_tx,
            event_rx,
            state: repeat(0).take(kind.key_count() as usize).collect(),
        };
        (deck, event_tx, command_rx)
    }

    #[tracing::instrument(skip_all)]
    pub async fn run(mut self) -> eyre::Result<()> {
        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    match event {
                        Some(Event::ButtonTap(button)) => {
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
                on_tap.invoke(self, &mut button_guard).await?;
            }
            self.command_tx.send(Command::Refresh).await?;
        } else {
            debug!("Button tap event received, but no handler set");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Event {
    ButtonTap(ButtonRef),
}

pub enum Command {
    Refresh,
    PushPage(Vec<Option<ButtonRef>>),
    PopPage,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Refresh => f.write_str("Refresh"),
            Command::PushPage(_) => f.write_str("PushPage"),
            Command::PopPage => f.write_str("PopPage"),
        }
    }
}
