use crate::config;
use crate::config::Config;
use crate::daemon::audio::{AudioCommand, AudioEvent, Track};
use crate::daemon::ui::btn::{Button, ButtonBehavior};
use elgato_streamdeck::info::Kind;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::default::Default;
use std::iter::repeat;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Result of button behavior execution, indicating whether display refresh should be skipped
#[derive(Debug, Clone, Default)]
pub(in crate::daemon::ui) struct BtnInvokeStatus {
    pub skip_refresh: bool,
}

mod btn;

pub use btn::ButtonRef;

async fn btn_pop(deck: &mut NoiseDeck) -> eyre::Result<BtnInvokeStatus> {
    if deck.view_stack.len() <= 1 {
        debug!("ignoring pop at home page");
        return Ok(BtnInvokeStatus::default());
    }

    deck.view_stack.pop();
    deck.display_top_page().await?;

    let mut result = BtnInvokeStatus::default();
    result.skip_refresh = true; // display_top_page() already sent UiCommand::Flip
    Ok(result)
}

async fn btn_push(deck: &mut NoiseDeck, id: Uuid) -> eyre::Result<BtnInvokeStatus> {
    deck.view_stack.push(View::new(id));
    deck.display_top_page().await?;

    let mut result = BtnInvokeStatus::default();
    result.skip_refresh = true; // display_top_page() already sent UiCommand::Flip
    Ok(result)
}

async fn btn_goto(deck: &mut NoiseDeck, id: Uuid) -> eyre::Result<BtnInvokeStatus> {
    deck.view_stack.clear();
    btn_push(deck, id).await
}

async fn btn_rotate(deck: &mut NoiseDeck) -> eyre::Result<BtnInvokeStatus> {
    let geo = deck.geo;

    // tracks
    let view = deck.current_view()?;
    let page = deck.get_library_category(&view.page_id.clone())?.to_vec();
    let page_len = page.len();
    let view = deck.current_view()?;
    let (_, n_displayed) = deck.layout_page(&page, view);
    let view = deck.current_view_mut()?;
    view.offset += geo.n_content.max(n_displayed);
    if view.offset >= page_len {
        view.offset = 0;
    }

    // playing
    deck.playing.offset += geo.n_dynamic;
    if deck.playing.offset >= deck.playing.buttons.len() {
        deck.playing.offset = 0;
    }

    deck.display_top_page().await?;

    let mut result = BtnInvokeStatus::default();
    result.skip_refresh = true; // display_top_page() already sent UiCommand::Flip
    Ok(result)
}

async fn btn_reset_offset(deck: &mut NoiseDeck) -> eyre::Result<BtnInvokeStatus> {
    // tracks
    let view = deck.current_view_mut()?;
    view.offset = 0;

    // playing
    deck.playing.offset = 0;

    deck.display_top_page().await?;

    let mut result = BtnInvokeStatus::default();
    result.skip_refresh = true; // display_top_page() already sent UiCommand::Flip
    Ok(result)
}

async fn btn_play_stop(deck: &mut NoiseDeck, track: &Arc<Track>) -> eyre::Result<BtnInvokeStatus> {
    let state = track.read().await;
    let track = track.clone();
    deck.audio_command_tx
        .send(if state.playback.is_advancing() {
            AudioCommand::Stop(track)
        } else {
            AudioCommand::Play(track)
        })
        .await?;

    Ok(BtnInvokeStatus::default())
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
    playing: PlayingView,
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

#[derive(Debug, Default)]
pub struct PlayingView {
    buttons: Vec<ButtonRef>,
    offset: usize,
}

impl PlayingView {
    /// Updates the playing list and indicates whether there was a change.
    pub fn update_playing(&mut self, button: &ButtonRef, playing: bool) -> bool {
        let currently_in_playing = self.buttons.contains(button);
        if playing && !currently_in_playing {
            self.buttons.push(button.clone());
            true
        } else if !playing && currently_in_playing {
            self.buttons.retain(|b| button != b);
            true
        } else {
            false
        }
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
            playing: Default::default(),
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

    fn layout_page(
        &self,
        semantic_buttons: &[ButtonRef],
        view: &View,
    ) -> (Vec<Option<ButtonRef>>, usize) {
        let mut page = Vec::with_capacity(self.kind.key_count().into());

        // Content (use skip and take for more resilience against out of bounds offsets
        let mut n_selected_buttons = 0usize;
        page.extend(
            semantic_buttons
                .iter()
                .skip(view.offset)
                .take(self.geo.n_content)
                .map(|b| Some(b.clone()))
                .pad_alt_cnt(self.geo.n_content, repeat(None), &mut n_selected_buttons),
        );

        // Back
        page.push(Some(
            Button::builder()
                .data(ButtonData {
                    label: "Back".to_string().into(),
                    ..Default::default()
                })
                .on_tap(ButtonBehavior::Pop)
                .on_hold(ButtonBehavior::Goto(self.config.start_page))
                .build()
                .into(),
        ));

        // Dynamic
        let mut effective_n_dyn_buttons = 0usize;
        page.extend(
            self.playing
                .buttons
                .iter()
                // skip is resilient against out of bounds offsets
                .skip(self.playing.offset)
                .chain(self.playing.buttons.iter().take(self.playing.offset))
                .filter(|b| {
                    !semantic_buttons
                        .iter()
                        .skip(view.offset)
                        .take(n_selected_buttons)
                        .any(|sb| sb == *b)
                })
                .take(self.geo.n_dynamic)
                .pad_alt_cnt(
                    self.geo.n_dynamic,
                    semantic_buttons
                        .iter()
                        .skip(view.offset + n_selected_buttons)
                        .filter(|b| !self.playing.buttons.contains(b)),
                    &mut effective_n_dyn_buttons,
                )
                .map(|b| Some(b.clone()))
                .pad(self.geo.n_dynamic, None),
        );
        n_selected_buttons += self
            .geo
            .n_dynamic
            .saturating_sub(effective_n_dyn_buttons)
            .min(
                semantic_buttons
                    .len()
                    .saturating_sub(view.offset)
                    .saturating_sub(n_selected_buttons),
            );

        // Next
        let page_size_estimate =
            self.geo.n_content + self.geo.n_dynamic.saturating_sub(effective_n_dyn_buttons);
        let total_n_pages = semantic_buttons.len() / page_size_estimate
            + (if semantic_buttons.len() % page_size_estimate > 0 {
                1
            } else {
                0
            });
        let current_page = view.offset / self.geo.n_content + 1;
        page.push(Some(
            Button::builder()
                .data(ButtonData {
                    label: format!(
                        "Next\n{current_page}/{total_n_pages}\n{page_size_estimate}/{}",
                        semantic_buttons.len()
                    )
                    .into(),
                    ..Default::default()
                })
                .on_tap(ButtonBehavior::Rotate)
                .on_hold(if view.offset == 0 && self.playing.offset == 0 {
                    ButtonBehavior::Rotate
                } else {
                    ButtonBehavior::ResetOffset
                })
                .build()
                .into(),
        ));

        debug_assert_eq!(page.len(), self.kind.key_count() as usize);
        (page, n_selected_buttons)
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
        let (physical_buttons, _) = self.layout_page(&semantic_buttons, self.current_view()?);
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
                    config::ButtonBehavior::PlaySound(path, settings) => Button::builder()
                        .data(ButtonData {
                            label: b.label.clone(),
                            ..Default::default()
                        })
                        .on_tap(ButtonBehavior::PlayStop)
                        .track(Arc::new(PathBuf::from(&path[..])), settings)
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
                        Some(UiEvent::ButtonHold(button)) => {
                            if let Err(e) = self.handle_button_hold(&button).await {
                                warn!(error = %e, "Error handling button hold event");
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
        let refresh_needed = {
            let mut btn_state = btn.inner.data.write().await;
            let track_state = track.read().await;
            btn_state.notification = if track_state.playback.is_advancing() {
                if let Some(remaining) = track_state.rem_duration {
                    let s = remaining.as_secs_f64();
                    let m = (s / 60.0).floor();
                    let s = s - m * 60.0;
                    Some(format!(" {:0.0}:{:.1}", m, s))
                } else {
                    Some("▶️".to_string())
                }
            } else {
                None
            };
            drop(btn_state);

            // update playing list
            if self
                .playing
                .update_playing(btn, track_state.playback.is_advancing())
            {
                self.display_top_page().await?;
                false
            } else {
                true
            }
        };

        if refresh_needed {
            self.ui_command_tx.send(UiCommand::Refresh).await?;
        }
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "trace")]
    async fn handle_button_tap(&mut self, button: &ButtonRef) -> eyre::Result<()> {
        if let Some(on_tap) = button.inner.on_tap.as_ref() {
            let result = {
                let mut button_guard = button.inner.data.write().await;
                on_tap
                    .invoke(self, &button.inner, &mut button_guard)
                    .await?
            };
            if !result.skip_refresh {
                self.ui_command_tx.send(UiCommand::Refresh).await?;
            }
        } else {
            debug!("Button tap event received, but no handler set");
        }
        Ok(())
    }

    #[tracing::instrument(skip(self), level = "trace")]
    async fn handle_button_hold(&mut self, button: &ButtonRef) -> eyre::Result<()> {
        if let Some(on_hold) = button.inner.on_hold.as_ref() {
            {
                let mut button_guard = button.inner.data.write().await;
                on_hold
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
use crate::util::IterExt;
pub use iface::{UiCommand, UiEvent};

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    mod harness;

    use harness::{BACK_BUTTON_LABEL, NAV_BUTTON_LABEL, SOUND_BUTTON_LABEL, with_test_harness};

    #[tokio::test]
    async fn test_back_button_navigation() -> eyre::Result<()> {
        with_test_harness(async |harness| {
            harness.tap_button(NAV_BUTTON_LABEL).await?;
            harness.expect_navigation().await?;
            harness
                .expect_on_page_with_button(BACK_BUTTON_LABEL)
                .await?;

            harness.tap_button(BACK_BUTTON_LABEL).await?;
            harness.expect_navigation().await?;
            harness.expect_on_page_with_button(NAV_BUTTON_LABEL).await?;

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_button_tap_navigation() -> eyre::Result<()> {
        with_test_harness(async |harness| {
            harness.tap_button(NAV_BUTTON_LABEL).await?;
            harness.expect_navigation().await?;
            harness
                .expect_on_page_with_button(BACK_BUTTON_LABEL)
                .await?;
            harness.expect_no_audio_commands().await?;

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_sound_button_tap_sends_audio_command() -> eyre::Result<()> {
        with_test_harness(async |harness| {
            harness.tap_button(NAV_BUTTON_LABEL).await?;
            harness.expect_navigation().await?;
            harness
                .expect_on_page_with_button(SOUND_BUTTON_LABEL)
                .await?;

            harness.tap_button(SOUND_BUTTON_LABEL).await?;

            let audio_command = harness.expect_audio_command().await?;
            assert_matches!(audio_command, crate::daemon::audio::AudioCommand::Play(_));
            harness.expect_refresh().await?;

            Ok(())
        })
        .await
    }
}
