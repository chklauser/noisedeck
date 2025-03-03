use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use eyre::Context;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{error, info, instrument};

pub struct Track {
    pub path: Arc<PathBuf>,
    state: RwLock<TrackState>,
}

impl Track {
    pub fn new(path: Arc<PathBuf>) -> Self {
        Track {
            path,
            state: RwLock::default(),
        }
    }
}

#[derive(Default)]
pub struct TrackState {
    sink: Option<Sink>,
}

pub enum AudioEvent {}

pub enum AudioCommand {
    Play(Arc<Track>),
}

struct AudioState {
    stream: OutputStream,
    stream_handle: OutputStreamHandle,
    tracks: Vec<Arc<Track>>,
    event_tx: Sender<AudioEvent>,
}
impl AudioState {
    pub fn new(event_tx: Sender<AudioEvent>) -> eyre::Result<Self> {
        let (stream, stream_handle) =
            OutputStream::try_default().context("Unable to create audio device")?;
        Ok(AudioState {
            stream,
            stream_handle,
            tracks: Vec::new(),
            event_tx,
        })
    }

    #[instrument(skip_all, level = "debug")]
    pub fn play(&mut self, track: Arc<Track>) -> eyre::Result<()> {
        let sink = Sink::try_new(&self.stream_handle).context("Unable to create audio sink")?;
        let mut track_state_guard = track.state.blocking_write();
        
        let file = BufReader::new(File::open(&*track.path)
            .with_context(|| format!("Unable to open {:?}", &track.path))?);
        let source = Decoder::new_mp3(file)
            .with_context(|| format!("Unable to decode {:?}", &track.path))?;
        sink.append(source);
        track_state_guard.sink = Some(sink);
        
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

#[instrument(skip_all)]
pub fn run(
    event_tx: Sender<AudioEvent>,
    mut command_rx: Receiver<AudioCommand>,
) -> eyre::Result<()> {
    let mut state = AudioState::new(event_tx)?;
    loop {
        let command = command_rx.blocking_recv();
        match command {
            Some(AudioCommand::Play(track)) => {
                if let Err(e) = state.play(track) {
                    error!("Error playing track: {:?}", e);
                }
            }
            None => {
                info!("Audio command channel closed, shutting down");
                state.shutdown();
                return Ok(());
            }
        }
    }
}
