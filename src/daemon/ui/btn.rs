use crate::config::PlaySoundSettings;
use crate::daemon::audio::Track;
use crate::daemon::ui::{
    BtnInvokeStatus, ButtonData, NoiseDeck, btn_goto, btn_play_stop, btn_pop, btn_push,
    btn_reset_offset, btn_rotate, btn_volume_up, btn_volume_down, btn_show_volume_control,
};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use tracing::warn;
use uuid::Uuid;

#[derive(Default)]
pub struct Button {
    pub(in crate::daemon::ui) data: tokio::sync::RwLock<ButtonData>,
    pub(in crate::daemon::ui) track: Option<Arc<Track>>,
    pub(in crate::daemon::ui) on_tap: Option<ButtonBehavior>,
    pub(in crate::daemon::ui) on_hold: Option<ButtonBehavior>,
}
impl Button {
    pub(in crate::daemon::ui) fn builder() -> ButtonBuilder {
        ButtonBuilder {
            inner: Button::default(),
        }
    }

    pub fn none() -> ButtonRef {
        static NONE: LazyLock<ButtonRef> = LazyLock::new(|| Button::builder().build().into());
        NONE.clone()
    }
}
pub(in crate::daemon::ui) struct ButtonBuilder {
    inner: Button,
}

pub(in crate::daemon::ui) enum ButtonBehavior {
    Push(Uuid),
    PlayStop,
    Pop,
    Goto(Uuid),
    Rotate,
    ResetOffset,
    VolumeUp,
    VolumeDown,
    ShowVolumeControl,
}
impl ButtonBehavior {
    pub(in crate::daemon::ui) async fn invoke(
        &self,
        deck: &mut NoiseDeck,
        button: &Button,
        _data: &mut ButtonData,
    ) -> eyre::Result<BtnInvokeStatus> {
        match self {
            ButtonBehavior::Pop => btn_pop(deck).await,
            ButtonBehavior::Push(id) => btn_push(deck, *id).await,
            ButtonBehavior::Goto(id) => btn_goto(deck, *id).await,
            ButtonBehavior::PlayStop => {
                if let Some(track) = &button.track {
                    btn_play_stop(deck, track).await
                } else {
                    warn!("Button has no track assigned");
                    Ok(BtnInvokeStatus::default())
                }
            }
            ButtonBehavior::Rotate => btn_rotate(deck).await,
            ButtonBehavior::ResetOffset => btn_reset_offset(deck).await,
            ButtonBehavior::VolumeUp => btn_volume_up(deck).await,
            ButtonBehavior::VolumeDown => btn_volume_down(deck).await,
            ButtonBehavior::ShowVolumeControl => btn_show_volume_control(deck).await,
        }
    }
}

impl ButtonBuilder {
    pub fn on_tap(mut self, behavior: ButtonBehavior) -> Self {
        self.inner.on_tap = Some(behavior);
        self
    }

    pub fn on_hold(mut self, behavior: ButtonBehavior) -> Self {
        self.inner.on_hold = Some(behavior);
        self
    }

    pub fn data(mut self, data: ButtonData) -> Self {
        *self.inner.data.get_mut() = data;
        self
    }

    pub fn track(mut self, track_path: Arc<PathBuf>, settings: &PlaySoundSettings) -> Self {
        self.inner.track = Some(Arc::new(Track::new(track_path, settings.clone())));
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

#[derive(Clone)]
pub struct ButtonRef {
    pub(in crate::daemon::ui) inner: Arc<Button>,
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
