use crate::daemon::audio::BlockingAudioCommand::AsyncCommand;
use eyre::Context;
use rodio::decoder::DecoderBuilder;
use rodio::{OutputStream, Sink, Source};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::MissedTickBehavior;
use tracing::{error, info, instrument, trace};

pub struct Track {
    pub path: Arc<PathBuf>,
    state: RwLock<TrackState>,
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Track").field("path", &self.path).finish()
    }
}

impl Track {
    pub fn new(path: Arc<PathBuf>) -> Self {
        Track {
            path,
            state: RwLock::default(),
        }
    }

    pub async fn read(&self) -> TrackState {
        TrackState {
            sink: None,
            ..*self.state.read().await
        }
    }
}

#[derive(Default)]
pub struct TrackState {
    sink: Option<Sink>,
    pub is_playing: bool,
    pub pct_played: f32,
    pub duration: Option<Duration>,
}

impl TrackState {
    pub fn rem_duration(&self) -> Option<Duration> {
        self.duration.map(|d| {
            let played = d.mul_f32(self.pct_played);
            d.checked_sub(played).unwrap_or_default()
        })
    }
}

pub enum AudioEvent {
    TrackStateChanged(Arc<Track>),
}

pub enum AudioCommand {
    Play(Arc<Track>),
    Stop(Arc<Track>),
}

pub enum BlockingAudioCommand {
    AsyncCommand(AudioCommand),
    UpdateState,
}

struct AudioState {
    stream: OutputStream,
    tracks: Vec<Arc<Track>>,
    event_tx: Sender<AudioEvent>,
}
impl AudioState {
    pub fn new(event_tx: Sender<AudioEvent>) -> eyre::Result<Self> {
        let stream = rodio::OutputStreamBuilder::open_default_stream()
            .context("Unable to create audio device")?;
        Ok(AudioState {
            stream,
            tracks: Vec::new(),
            event_tx,
        })
    }

    #[instrument(skip_all, level = "debug")]
    fn play(&mut self, track: Arc<Track>) -> eyre::Result<()> {
        if self.tracks.iter().any(|t| Arc::ptr_eq(&track, t)) {
            info!("Track {:?} already playing, not changing anything", &track);
            return Ok(());
        }

        let sink = Sink::connect_new(&self.stream.mixer());
        let mut track_state_guard = track.state.blocking_write();

        let total_duration = {
            let file = File::open(&*track.path)
                .with_context(|| format!("Unable to open {:?}", &track.path))?;
            let file_len = file.metadata()?.len();
            let source = DecoderBuilder::new()
                .with_data(BufReader::with_capacity(512 * 1024, file))
                .with_hint("mp3")
                .with_byte_len(file_len)
                .with_gapless(true)
                .with_seekable(true)
                .build()
                .with_context(|| format!("Unable to decode {:?}", &track.path))?;
            source.total_duration()
        };

        let file = File::open(&*track.path)
            .with_context(|| format!("Unable to open {:?}", &track.path))?;
        let file_len = file.metadata()?.len();
        let source = DecoderBuilder::new()
            .with_data(BufReader::with_capacity(512 * 1024, file))
            .with_hint("mp3")
            .with_byte_len(file_len)
            .with_gapless(true)
            .with_seekable(true)
            .build_looped()
            .with_context(|| format!("Unable to decode {:?}", &track.path))?;
        let source = source.fade_in(Duration::from_secs(2));
        sink.append(source);
        track_state_guard.sink.replace(sink);
        track_state_guard.pct_played = 0.0;
        track_state_guard.is_playing = true;
        track_state_guard.duration = total_duration;

        self.tracks.push(track.clone());
        Ok(())
    }

    #[instrument(skip_all, level = "debug")]
    pub fn shutdown(self) {
        for track in self.tracks {
            let mut track_state_guard = track.state.blocking_write();
            if let Some(sink) = &mut track_state_guard.sink {
                sink.stop();
            }
            track_state_guard.sink = None;
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
                let mut track_state_guard = track.state.blocking_write();
                if let Some(sink) = &mut track_state_guard.sink {
                    sink.stop();
                }
                track_state_guard.sink = None;
                track_state_guard.pct_played = 0.0;
                track_state_guard.is_playing = false;
                drop(track_state_guard);

                state.tracks.retain(|t| !Arc::ptr_eq(&track, t));
                update_track_state(track, &state.event_tx)?
            }
            BlockingAudioCommand::UpdateState => {
                for track in state.tracks.iter() {
                    update_track_state(track.clone(), &state.event_tx)?
                }
            }
        }
    }

    info!("Audio command channel closed, shutting down");
    state.shutdown();
    Ok(())
}

fn update_track_state(track: Arc<Track>, event_tx: &Sender<AudioEvent>) -> eyre::Result<()> {
    let mut state = track.state.blocking_write();
    if let Some(sink) = &state.sink {
        if let Some(duration) = state.duration {
            let multiples = sink.get_pos().as_millis() as f64 / duration.as_millis() as f64;
            state.pct_played = (multiples - multiples.floor()).clamp(0.0, 1.0) as f32;
        }
    } else {
        state.pct_played = 0.0;
    };

    drop(state);
    event_tx.blocking_send(AudioEvent::TrackStateChanged(track.clone()))?;
    Ok(())
}
