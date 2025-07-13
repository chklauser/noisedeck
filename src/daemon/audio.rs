use crate::config::PlaySoundSettings;
use crate::daemon::audio::BlockingAudioCommand::AsyncCommand;
use eyre::Context;
use kira::effect::volume_control::VolumeControlHandle;
use kira::sound::streaming::{StreamingSoundData, StreamingSoundHandle};
use kira::sound::{FromFileError, PlaybackState};
use kira::{AudioManager, AudioManagerSettings, DefaultBackend, Decibels, Easing, Tween};
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::MissedTickBehavior;
use tracing::{error, info, instrument, trace};

pub struct Track {
    pub path: Arc<PathBuf>,
    pub settings: PlaySoundSettings,
    state: Mutex<Box<dyn TrackState>>,
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Track").field("path", &self.path).finish()
    }
}

impl Track {
    pub fn new(path: Arc<PathBuf>, settings: PlaySoundSettings) -> Self {
        Self::with_state(path, settings, Box::<RealTrackState>::default())
    }

    pub fn with_state(
        path: Arc<PathBuf>,
        settings: PlaySoundSettings,
        state: Box<dyn TrackState>,
    ) -> Self {
        Track {
            path,
            settings,
            state: Mutex::new(state),
        }
    }

    pub async fn read(&self) -> TrackStateData {
        let guard = self.state.lock().await;
        TrackStateData {
            rem_duration: guard.rem_duration(),
            playback: guard.playback_state(),
        }
    }

    #[cfg(test)]
    pub async fn update_mock_state(&self, playback: PlaybackState) -> eyre::Result<()> {
        use crate::daemon::ui::tests::harness::MockTrackState;
        
        let mut guard = self.state.lock().await;
        let mock_state = guard
            .as_any_mut()
            .downcast_mut::<MockTrackState>()
            .ok_or_else(|| eyre::eyre!("Expected MockTrackState in test"))?;
        mock_state.playback = playback;
        Ok(())
    }
}

pub trait TrackState: Send {
    fn rem_duration(&self) -> Option<Duration>;
    fn playback_state(&self) -> PlaybackState;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[derive(Default)]
pub struct RealTrackState {
    pub sink: Option<StreamingSoundHandle<FromFileError>>,
    pub duration: Option<Duration>,
}

impl TrackState for RealTrackState {
    fn rem_duration(&self) -> Option<Duration> {
        self.duration.zip(self.sink.as_ref()).map(|(d, h)| {
            let played = Duration::from_secs_f64(h.position());
            d.checked_sub(played).unwrap_or_default()
        })
    }

    fn playback_state(&self) -> PlaybackState {
        self.sink
            .as_ref()
            .map(|s| s.state())
            .unwrap_or(PlaybackState::Stopped)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

pub struct TrackStateData {
    pub rem_duration: Option<Duration>,
    pub playback: PlaybackState,
}

impl<T: TrackState + ?Sized> From<&T> for TrackStateData {
    fn from(state: &T) -> Self {
        TrackStateData {
            rem_duration: state.rem_duration(),
            playback: state.playback_state(),
        }
    }
}

pub enum AudioEvent {
    TrackStateChanged(Arc<Track>),
    GlobalVolumeChanged(f64),
}

#[derive(Debug)]
pub enum AudioCommand {
    Play(Arc<Track>),
    Stop(Arc<Track>),
    SetGlobalVolume(f64),
    GetGlobalVolume,
}

pub enum BlockingAudioCommand {
    AsyncCommand(AudioCommand),
    UpdateState,
}

struct AudioState {
    manager: AudioManager,
    tracks: Vec<Arc<Track>>,
    event_tx: Sender<AudioEvent>,
    global_volume: VolumeControlHandle,
    current_volume_db: f64,
}
impl AudioState {
    pub fn new(event_tx: Sender<AudioEvent>) -> eyre::Result<Self> {
        let mut settings = AudioManagerSettings::default();
        let global_volume = settings
            .main_track_builder
            .add_effect(kira::effect::volume_control::VolumeControlBuilder::default());
        let manager = AudioManager::<DefaultBackend>::new(settings)
            .context("Unable to create audio device")?;
        Ok(AudioState {
            manager,
            global_volume,
            tracks: Vec::new(),
            event_tx,
            current_volume_db: 0.0, // Start at 0 dB (no change)
        })
    }

    #[instrument(skip_all, level = "debug")]
    fn set_global_volume(&mut self, volume_db: f64) -> eyre::Result<()> {
        self.global_volume.set_volume(
            Decibels(volume_db as f32),
            Tween::default(),
        );
        self.current_volume_db = volume_db;
        self.event_tx.blocking_send(AudioEvent::GlobalVolumeChanged(volume_db))?;
        Ok(())
    }

    #[instrument(skip_all, level = "debug")]
    fn get_global_volume(&mut self) -> eyre::Result<f64> {
        // Return the current tracked volume
        let volume_db = self.current_volume_db;
        self.event_tx.blocking_send(AudioEvent::GlobalVolumeChanged(volume_db))?;
        Ok(volume_db)
    }

    #[instrument(skip_all, level = "debug")]
    fn play(&mut self, track: Arc<Track>) -> eyre::Result<()> {
        if !track.settings.mode.overlaps() && self.tracks.iter().any(|t| Arc::ptr_eq(&track, t)) {
            info!("Track {:?} already playing, not changing anything", &track);
            return Ok(());
        }

        let mut track_state_guard = track.state.blocking_lock();
        let mut sound_data =
            StreamingSoundData::from_file(track.path.as_path()).with_context(|| {
                format!(
                    "Failed to load sound data from path {}",
                    &track.path.display()
                )
            })?;
        let total_duration = sound_data.duration();
        if let Some(fade_in) = track.settings.fade_in {
            sound_data = sound_data.fade_in_tween(Tween {
                duration: fade_in,
                easing: Easing::OutPowi(2),
                ..Default::default()
            });
        }
        let mut track_handle = self
            .manager
            .play(sound_data)
            .with_context(|| format!("Failed to play {:?}", &track.path))?;
        if track.settings.mode.loops() {
            track_handle.set_loop_region(..);
        }

        let state = track_state_guard
            .as_any_mut()
            .downcast_mut::<RealTrackState>()
            .expect("invalid track state type");
        state.sink = Some(track_handle);
        state.duration = Some(total_duration);

        self.tracks.push(track.clone());
        Ok(())
    }

    #[instrument(skip_all, level = "debug")]
    pub fn shutdown(self) {
        for track in self.tracks {
            let mut track_state_guard = track.state.blocking_lock();
            let state = track_state_guard
                .as_any_mut()
                .downcast_mut::<RealTrackState>()
                .expect("invalid track state type");
            if let Some(sink) = &mut state.sink {
                sink.stop(Tween {
                    duration: Duration::default(),
                    ..Default::default()
                })
            }
            state.sink = None;
        }
    }
}

pub async fn run(
    event_tx: Sender<AudioEvent>,
    mut command_rx: Receiver<AudioCommand>,
) -> eyre::Result<()> {
    let (blocking_cmd_tx, blocking_cmd_rx) = std::sync::mpsc::channel::<BlockingAudioCommand>();
    let interrupt_task = tokio::task::spawn(async move {
        let mut timeout = tokio::time::interval(Duration::from_millis(500));
        timeout.set_missed_tick_behavior(MissedTickBehavior::Delay);
        'task: loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some(command) = command else {
                        trace!("Audio command channel closed, shutting down translation loop");
                        break 'task;
                    };
                    if blocking_cmd_tx.send(AsyncCommand(command)).is_err() {
                        trace!("Blocking audio command channel closed, shutting down translation loop (a)");
                        break 'task;
                    }
                },
                _ = timeout.tick() => {
                    trace!("ask for audio state update");
                    if blocking_cmd_tx.send(BlockingAudioCommand::UpdateState).is_err() {
                        trace!("Blocking audio command channel closed, shutting down translation loop (i)");
                        break 'task;
                    }
                }
            }
        }
    });

    let sync_thread_finished =
        tokio::task::spawn_blocking(move || run_sync(event_tx, blocking_cmd_rx));

    sync_thread_finished.await??;
    interrupt_task.await?;
    Ok(())
}

#[instrument(skip_all)]
fn run_sync(
    event_tx: Sender<AudioEvent>,
    command_rx: std::sync::mpsc::Receiver<BlockingAudioCommand>,
) -> eyre::Result<()> {
    let mut state = AudioState::new(event_tx)?;
    while let Ok(command) = command_rx.recv() {
        match command {
            AsyncCommand(AudioCommand::Play(track)) => {
                if let Err(e) = state.play(track) {
                    error!("Error playing track: {:?}", e);
                }
            }
            AsyncCommand(AudioCommand::Stop(track)) => {
                let mut track_state_guard = track.state.blocking_lock();
                let track_state = track_state_guard
                    .as_any_mut()
                    .downcast_mut::<RealTrackState>()
                    .expect("invalid track state type");
                if let Some(sink) = &mut track_state.sink {
                    sink.stop(Tween {
                        duration: Duration::from_millis(2000),
                        easing: Easing::InPowi(2),
                        ..Default::default()
                    });
                }
                track_state.sink = None;
                drop(track_state_guard);

                state.tracks.retain(|t| !Arc::ptr_eq(&track, t));
                update_track_state(track, &state.event_tx)?
            }
            AsyncCommand(AudioCommand::SetGlobalVolume(volume_db)) => {
                if let Err(e) = state.set_global_volume(volume_db) {
                    error!("Error setting global volume: {:?}", e);
                }
            }
            AsyncCommand(AudioCommand::GetGlobalVolume) => {
                if let Err(e) = state.get_global_volume() {
                    error!("Error getting global volume: {:?}", e);
                }
            }
            BlockingAudioCommand::UpdateState => {
                let mut idx_to_remove = Vec::new();
                for (idx, track) in state.tracks.iter().enumerate() {
                    let state_guard = track.state.blocking_lock();
                    let track_state = state_guard
                        .as_any()
                        .downcast_ref::<RealTrackState>()
                        .expect("invalid track state type");
                    if let Some(sink) = &track_state.sink {
                        if sink.state() == PlaybackState::Stopped {
                            idx_to_remove.push(idx);
                        }
                    }
                    drop(state_guard);
                    update_track_state(track.clone(), &state.event_tx)?;
                }

                // swap remove is only safe in reverse order (idx_to_remove is sorted asc)
                for idx in idx_to_remove.into_iter().rev() {
                    state.tracks.swap_remove(idx);
                }
            }
        }
    }

    info!("Audio command channel closed, shutting down");
    state.shutdown();
    Ok(())
}

fn update_track_state(track: Arc<Track>, event_tx: &Sender<AudioEvent>) -> eyre::Result<()> {
    event_tx.blocking_send(AudioEvent::TrackStateChanged(track.clone()))?;
    Ok(())
}
