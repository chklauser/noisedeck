use crate::config::{ButtonBehavior, Config, Page};
use crate::daemon::ui::{ButtonData, ButtonRef, UiCommand};
use crate::import::ImportArgs;
use clap::Args;
use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache, Weight};
use elgato_streamdeck::asynchronous::list_devices_async;
use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, new_hidapi};
use eyre::{Context, ContextCompat, OptionExt, Report};
use image::{DynamicImage, ImageBuffer, Rgb};
use imageproc::image::RgbImage;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, trace, warn};

mod audio;
mod ui;

#[derive(Debug, Eq, PartialEq, Args, Clone)]
pub struct DaemonArgs {
    #[command(flatten)]
    import: ImportArgs,

    #[arg(long, env = "audio_path")]
    audio_path: PathBuf,

    #[arg(long, env = "check_paths")]
    check_paths: bool,
}

#[tracing::instrument(skip(args))]
pub async fn run(args: DaemonArgs) -> Result<(), eyre::Error> {
    let hid = new_hidapi().context("Failed to create HIDAPI")?;
    let devices = list_devices_async(&hid);
    info!("Found {} devices", devices.len());
    let (kind, serial) = devices
        .iter()
        .find(|(kind, _)| *kind == Kind::Original || *kind == Kind::OriginalV2)
        .ok_or_eyre("No supported StreamDeck found")?;

    let device = AsyncStreamDeck::connect(&hid, *kind, serial)
        .with_context(|| format!("Failed to connect to device {:?} {}", kind, &serial))?;
    debug!(
        "Connected to '{}' with version '{}'. Key count {}",
        device.serial_number().await?,
        device.firmware_version().await?,
        kind.key_count()
    );

    device.set_brightness(60).await?;
    device.clear_all_button_images().await?;

    let config = Arc::new(
        tokio::task::spawn_blocking(move || match crate::import::run_sync(args.import.clone()) {
            Ok(mut config) => {
                rebase_paths(&args, &mut config)?;
                Ok(config)
            }
            e => e,
        })
        .await??,
    );

    let (mut deck, ui_event_tx, mut ui_command_rx, audio_event_tx, audio_command_rx) =
        ui::NoiseDeck::new(device.kind(), config.clone());
    deck.init().await?;
    let deck_finished = tokio::spawn(deck.run());
    let audio_player_finished = tokio::spawn(audio::run(audio_event_tx, audio_command_rx));

    let font_system = load_fonts().await?;
    let swash_cache = SwashCache::new();
    let mut state = DeckState {
        page: vec![],
        render_cache: vec![],
        font_system,
        swash_cache,
        device,
        event_tx: ui_event_tx,
    };

    let reader = state.device.get_reader();
    let sigint = tokio::signal::ctrl_c();
    tokio::pin!(sigint);

    'infinite: loop {
        tokio::select! {
            updates_result = reader.read(100.0) => {
                let updates = updates_result.context("Failed to read updates")?;
                match state.handle_updates(updates).await {
                    Ok(_) => {}
                    Err(e) => {
                        warn!(error = %e, "Error handling updates");
                        break 'infinite;
                    }
                }
            },
            command = ui_command_rx.recv() => {
                if let Some(command) = command {
                    match state.handle_command(command).await {
                        Ok(_) => {}
                        Err(e) => {
                            warn!(error = %e, "Error handling command");
                            break 'infinite;
                        }
                    }
                } else {
                    info!("Command channel closed");
                    break 'infinite
                }
            },
            sigint_result = &mut sigint => {
                match sigint_result {
                    Ok(_) => {
                        info!("Received SIGINT, shutting down gracefully");
                        break 'infinite;
                    }
                    Err(e) => {
                        warn!(error = %e, "Error waiting for SIGINT");
                        break 'infinite;
                    }
                }
            }
        }
    }
    drop(reader);
    let device = state.shutdown();
    if let Err(e) = deck_finished.await? {
        error!("Deck task failed: {}", e);
    }
    if let Err(e) = audio_player_finished.await? {
        error!("Audio player task failed: {}", e);
    }

    if device.shutdown().await.is_err() && device.sleep().await.is_err() {
        device.set_brightness(15).await?;
    }

    Ok(())
}

#[instrument(skip_all, level = "DEBUG")]
fn rebase_paths(args: &DaemonArgs, config: &mut Config) -> eyre::Result<()> {
    let mut buf = PathBuf::new();
    for (_, page) in config.pages.iter_mut() {
        let mut new_page: Page = (**page).clone();
        for b in new_page.buttons.iter_mut() {
            if let ButtonBehavior::PlaySound { path } = &mut b.behavior {
                buf.clear();
                buf.push(&args.audio_path);
                buf.push(&**path);
                if args.check_paths {
                    match std::fs::metadata(&buf) {
                        Ok(m) if m.is_file() => (),
                        Ok(m) => warn!("Path {} is not a file: {:?}", buf.display(), m.file_type()),
                        Err(e) => warn!("Error checking path {}: {}", buf.display(), e),
                    }
                }
                *path = buf
                    .to_str()
                    .with_context(|| {
                        format!("Rebased path is not valid UTF-8: '{:?}'", buf.display())
                    })?
                    .to_string()
                    .into();
            }
        }
        *page = Arc::new(new_page);
    }
    Ok(())
}

struct RenderCacheEntry {
    button: Option<ButtonData>,
}

struct DeckState {
    page: Vec<Option<ButtonRef>>,
    render_cache: Vec<Option<RenderCacheEntry>>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    device: AsyncStreamDeck,
    event_tx: tokio::sync::mpsc::Sender<ui::UiEvent>,
}

impl DeckState {
    fn shutdown(self) -> AsyncStreamDeck {
        self.device
    }

    #[instrument(skip(self), level = "TRACE")]
    async fn render_button_image(&mut self, button: &mut ButtonData) -> DynamicImage {
        let mut bg_color = Rgb([0u8, 0u8, 0u8]);
        let mut text_color = Rgb([0xFFu8, 0xFFu8, 0xFFu8]);
        if button.notification.is_some() {
            std::mem::swap(&mut bg_color, &mut text_color);
        };
        let mut image = RgbImage::from_pixel(72, 72, bg_color);
        let metrics = Metrics::new(16.0, 24.0);
        let text_color = Color::rgb(text_color.0[0], text_color.0[1], text_color.0[2]);

        self.render_text(
            &mut image,
            &button.label,
            metrics,
            bg_color,
            text_color,
            if button.notification.is_some() {
                Weight::NORMAL
            } else {
                Weight::EXTRA_BOLD
            },
            72,
        );
        if let Some(notification) = &button.notification {
            self.render_text(
                &mut image,
                notification,
                metrics,
                bg_color,
                text_color,
                Weight::EXTRA_BOLD,
                32,
            );
        }

        image.into()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_text(
        &mut self,
        image: &mut ImageBuffer<Rgb<u8>, Vec<u8>>,
        text: &str,
        metrics: Metrics,
        bg_color: Rgb<u8>,
        text_color: Color,
        weight: Weight,
        height: i32,
    ) {
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let mut buffer = buffer.borrow_with(&mut self.font_system);
        buffer.set_size(Some(70.0), Some((height - 2) as f32));
        let mut attrs = Attrs::new();
        attrs.weight = weight;
        buffer.set_text(text, attrs, Shaping::Advanced);

        buffer.shape_until_scroll(true);
        let swash_cache = &mut self.swash_cache;
        buffer.draw(swash_cache, text_color, |x, y, _w, _h, color| {
            let x = x + 1;
            let y = y + 1 + (72 - height);
            if x < 0 || y < 0 || x > 71 || y > 71 {
                if x < -1 || y < -1 || x > 72 || y > 72 {
                    warn!("Out of bounds: x: {}, y: {}", x, y);
                }
                return;
            }
            let alpha_f = color.a() as f32 / 255.0;
            let image_color_multiplied_alpha = Rgb([
                (color.r() as f32 * alpha_f + bg_color.0[0] as f32 * (1.0 - alpha_f)) as u8,
                (color.g() as f32 * alpha_f + bg_color.0[1] as f32 * (1.0 - alpha_f)) as u8,
                (color.b() as f32 * alpha_f + bg_color.0[2] as f32 * (1.0 - alpha_f)) as u8,
            ]);
            image.put_pixel(x as u32, y as u32, image_color_multiplied_alpha)
        });
    }

    #[instrument(skip(self), level = "TRACE")]
    pub async fn handle_command(&mut self, command: UiCommand) -> eyre::Result<()> {
        match command {
            UiCommand::Refresh => {
                let mut flush_required = false;
                for (i, button) in self
                    .page
                    .clone()
                    .into_iter()
                    .take(u8::MAX as usize)
                    .enumerate()
                {
                    let image = if let Some(r) = button.as_ref() {
                        let mut data = r.read().await;
                        if self
                            .render_cache
                            .get(i)
                            .and_then(|e| e.as_ref())
                            .map(|r| r.button.as_ref() == Some(&data))
                            .unwrap_or(false)
                        {
                            continue;
                        } else {
                            self.render_cache[i] = Some(RenderCacheEntry {
                                button: Some(data.clone()),
                            });
                            self.render_button_image(&mut data).await
                        }
                    } else if self
                        .render_cache
                        .get(i)
                        .and_then(|e| e.as_ref())
                        .map(|e| e.button.is_none())
                        .unwrap_or(false)
                    {
                        continue;
                    } else {
                        self.render_cache[i] = Some(RenderCacheEntry { button: None });
                        ImageBuffer::from_pixel(71, 71, Rgb([0u8, 0u8, 0u8])).into()
                    };
                    self.device.set_button_image(i as u8, image).await?;
                    flush_required = true;
                }

                if flush_required {
                    trace!("Flushing stream deck");
                    self.device.flush().await?;
                }
            }
            UiCommand::Flip(new_page) => {
                self.page = new_page;
                // TODO: Some flips are partial; be smarter about clearing cache entries
                self.render_cache.clear();
                self.render_cache.extend((0..self.page.len()).map(|_| None));
                Box::pin(self.handle_command(UiCommand::Refresh)).await?;
            }
        }
        Ok(())
    }

    fn button_by_key(&mut self, key: u8) -> eyre::Result<Option<ButtonRef>> {
        Ok(self.page.get::<usize>(key.into()).and_then(|b| b.clone()))
    }

    #[tracing::instrument(level = "TRACE", skip_all)]
    async fn handle_updates(&mut self, updates: Vec<DeviceStateUpdate>) -> Result<(), Report> {
        for update in updates {
            match update {
                DeviceStateUpdate::ButtonDown(key) => {
                    info!("Button {} down", key);
                }
                DeviceStateUpdate::ButtonUp(key) => {
                    info!("Button {} up", key);
                    if let Some(button) = self.button_by_key(key)? {
                        self.event_tx.send(ui::UiEvent::ButtonTap(button)).await?;
                    } else {
                        warn!("Button {} not found", key);
                    }
                }
                unknown => {
                    info!("Ignoring device update: {:?}", unknown);
                }
            };
        }
        Ok(())
    }
}

#[tracing::instrument(level = tracing::Level::DEBUG)]
async fn load_fonts() -> eyre::Result<FontSystem> {
    let emoji_font_data = Vec::from(include_bytes!(
        "../font/noto-color-emoji/NotoColorEmoji-NoSvg.ttf"
    ));
    let sans_font_data = Vec::from(include_bytes!(
        "../font/noto-sans/static/NotoSans-Medium.ttf"
    ));
    // let sans_font_data = Vec::from(include_bytes!("../font/noto-sans/static/NotoSans-Medium.ttf"));
    tokio::task::spawn_blocking(move || {
        // FontSystem::new_with_fonts(fonts)
        // FontSystem::new()
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_data(sans_font_data);
        db.load_font_data(emoji_font_data);
        db.set_sans_serif_family("Noto Sans".to_owned());
        db.set_serif_family("Noto Sans".to_owned());
        db.set_monospace_family("Noto Sans".to_owned());
        db.set_cursive_family("Noto Sans".to_owned());
        db.set_fantasy_family("Noto Sans".to_owned());
        FontSystem::new_with_locale_and_db("en-US".to_owned(), db)
    })
    .await
    .context("Failed to load fonts")
}
