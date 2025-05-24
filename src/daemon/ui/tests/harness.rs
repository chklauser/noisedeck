//! Testing harness for the `daemon::ui` test suite.
//!
//! Don't add tests here. Add tests in the `daemon::ui` module instead.

use crate::{
    config::{self, ButtonBehavior, Config, PlaySoundSettings, PlaybackMode},
    daemon::{
        audio::{AudioCommand, AudioEvent},
        ui::{ButtonRef, NoiseDeck, UiCommand, UiEvent},
    },
};
use assert_matches::assert_matches;
use elgato_streamdeck::info::Kind;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::mpsc::{Receiver, Sender},
    time::timeout,
};
use uuid::Uuid;

pub const NAV_BUTTON_LABEL: &str = "Go to Target";
pub const BACK_BUTTON_LABEL: &str = "Back";
pub const SOUND_BUTTON_LABEL: &str = "Play Sound";

pub struct TestHarness {
    pub ui_event_tx: Sender<UiEvent>,
    pub ui_command_rx: Receiver<UiCommand>,
    pub audio_command_rx: Receiver<AudioCommand>,
    pub audio_event_tx: Sender<AudioEvent>,
    pub deck_handle: tokio::task::JoinHandle<eyre::Result<()>>,
    pub current_buttons: Vec<Option<ButtonRef>>,
}

impl TestHarness {
    async fn new() -> eyre::Result<Self> {
        let (mut deck, ui_event_tx, mut ui_command_rx, audio_event_tx, audio_command_rx) = {
            let config = create_test_config();
            NoiseDeck::new(Kind::Mk2, config)
        };

        let deck_handle = tokio::spawn(async move {
            deck.init().await.unwrap();
            deck.run().await
        });

        // Consume initial display and store current buttons
        let initial_command = timeout(Duration::from_millis(100), ui_command_rx.recv())
            .await
            .expect("Should receive initial command")
            .expect("Should receive command");

        let current_buttons = match initial_command {
            UiCommand::Flip(buttons) => buttons,
            _ => panic!(
                "Expected initial UiCommand::Flip, got {:?}",
                initial_command
            ),
        };

        Ok(TestHarness {
            ui_event_tx,
            ui_command_rx,
            audio_command_rx,
            audio_event_tx,
            deck_handle,
            current_buttons,
        })
    }

    pub async fn tap_button(&mut self, label: &str) -> eyre::Result<()> {
        let button = self
            .find_button_by_label(label)
            .await
            .ok_or_else(|| eyre::eyre!("Button '{}' not found on current page", label))?;

        self.ui_event_tx.send(UiEvent::ButtonTap(button)).await?;
        Ok(())
    }

    pub async fn expect_navigation(&mut self) -> eyre::Result<()> {
        let command = timeout(Duration::from_millis(100), self.ui_command_rx.recv())
            .await
            .expect("Should receive UI command within timeout")
            .expect("Should receive UI command");

        match command {
            UiCommand::Flip(buttons) => {
                self.current_buttons = buttons;
            }
            _ => {
                return Err(eyre::eyre!(
                    "Expected Flip command for navigation, got {:?}",
                    command
                ));
            }
        }
        Ok(())
    }

    pub async fn expect_on_page_with_button(&self, label: &str) -> eyre::Result<()> {
        if self.find_button_by_label(label).await.is_none() {
            return Err(eyre::eyre!(
                "Expected to be on page with '{}' button",
                label
            ));
        }
        Ok(())
    }

    pub async fn expect_audio_command(&mut self) -> eyre::Result<AudioCommand> {
        timeout(Duration::from_millis(100), self.audio_command_rx.recv())
            .await
            .expect("Should receive audio command within timeout")
            .ok_or_else(|| eyre::eyre!("Audio command channel closed"))
    }

    pub async fn expect_refresh(&mut self) -> eyre::Result<()> {
        let command = timeout(Duration::from_millis(100), self.ui_command_rx.recv())
            .await
            .expect("Should receive UI command within timeout")
            .expect("Should receive UI command");

        assert_matches!(command, UiCommand::Refresh);
        Ok(())
    }

    pub async fn expect_no_audio_commands(&mut self) -> eyre::Result<()> {
        let result = timeout(Duration::from_millis(50), self.audio_command_rx.recv()).await;
        assert_matches!(result, Err(_)); // Timeout is expected - no commands
        Ok(())
    }

    pub async fn simulate_track_state_changed(&mut self, sound_path: &str) -> eyre::Result<()> {
        use crate::daemon::audio::{AudioEvent, Track};
        use std::path::PathBuf;

        let track = Arc::new(Track::new(
            Arc::new(PathBuf::from(sound_path)),
            PlaySoundSettings {
                volume: 0.8,
                mode: PlaybackMode::PlayStop,
                fade_in: Some(Duration::from_millis(100)),
                fade_out: Some(Duration::from_millis(100)),
            },
        ));

        // Send the track state changed event
        // The UI will call track.read() to get the current state
        // Since we can't mock the internal StreamingSoundHandle,
        // the track will appear as stopped (default state)
        self.audio_event_tx
            .send(AudioEvent::TrackStateChanged(track))
            .await?;
        Ok(())
    }

    async fn find_button_by_label(&self, label: &str) -> Option<ButtonRef> {
        for opt_btn in &self.current_buttons {
            if let Some(btn) = opt_btn {
                let button_data = btn.read().await;
                if button_data.label.as_str() == label {
                    return Some(btn.clone());
                }
            }
        }
        None
    }

    async fn cleanup(self) {
        drop(self.ui_event_tx);
        let _ = timeout(Duration::from_millis(100), self.deck_handle).await;
    }
}

/// Runs a test with automatic harness cleanup
pub async fn with_test_harness<F>(test_fn: F) -> eyre::Result<()>
where
    F: AsyncFn(&mut TestHarness) -> eyre::Result<()>,
{
    let mut harness = TestHarness::new().await?;
    let result = test_fn(&mut harness).await;
    harness.cleanup().await;
    result
}

fn create_test_config() -> Arc<Config> {
    let start_page = Uuid::from_u128(1);
    let target_page = Uuid::from_u128(2);

    let mut pages = HashMap::new();

    // Main page with a navigation button
    let main_page = config::Page {
        name: "Main".to_string(),
        buttons: vec![config::Button {
            label: Arc::new(NAV_BUTTON_LABEL.to_string()),
            behavior: ButtonBehavior::PushPage(target_page),
        }],
    };
    pages.insert(start_page, Arc::new(main_page));

    // Target page with a sound button
    let target_page_config = config::Page {
        name: "Target".to_string(),
        buttons: vec![config::Button {
            label: Arc::new(SOUND_BUTTON_LABEL.to_string()),
            behavior: ButtonBehavior::PlaySound(
                Arc::new("test_sound.mp3".to_string()),
                PlaySoundSettings {
                    volume: 0.8,
                    mode: PlaybackMode::PlayStop,
                    fade_in: Some(Duration::from_millis(100)),
                    fade_out: Some(Duration::from_millis(100)),
                },
            ),
        }],
    };
    pages.insert(target_page, Arc::new(target_page_config));

    Arc::new(Config { pages, start_page })
}
