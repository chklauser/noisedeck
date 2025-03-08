use crate::daemon::ui::{ButtonData, ButtonRef, UiCommand};
use crate::import::ImportArgs;
use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, Weight};
use elgato_streamdeck::asynchronous::list_devices_async;
use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, new_hidapi};
use eyre::{Context, OptionExt, Report};
use image::{DynamicImage, ImageBuffer, Rgb};
use imageproc::image::RgbImage;
use std::env;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

mod audio;
mod ui;

#[tracing::instrument]
pub async fn run() -> Result<(), eyre::Error> {
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
        tokio::task::spawn_blocking(|| {
            crate::import::run_sync(ImportArgs {
                path: env::var("import_path")?.into(),
                profile_name: env::var("profile_name")?,
            })
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

struct DeckState {
    page: Vec<Option<ButtonRef>>,
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
        let mut image = RgbImage::from_pixel(71, 71, Rgb([0u8, 0u8, 0u8]));
        let metrics = Metrics::new(16.0, 24.0);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let mut buffer = buffer.borrow_with(&mut self.font_system);
        buffer.set_size(Some(72.0), Some(72.0));
        let mut attrs = Attrs::new();
        attrs.weight = Weight::EXTRA_BOLD;
        buffer.set_text(
            button
                .notification
                .as_ref()
                .map(|s| &s[..])
                .unwrap_or(&button.label),
            attrs,
            Shaping::Advanced,
        );
        //buffer.set_text("Hello World, fine däy, eh?", attrs, Shaping::Advanced);
        buffer.shape_until_scroll(true);
        let text_color = cosmic_text::Color::rgb(0xFF, 0xFF, 0xFF);
        buffer.draw(&mut self.swash_cache, text_color, |x, y, _w, _h, color| {
            if x < 0 || y < 0 || x >= 71 || y >= 71 {
                if x < -1 || y < -1 || x > 71 || y > 71 {
                    warn!("Out of bounds: x: {}, y: {}", x, y);
                }
                return;
            }
            let alpha_f = color.a() as f32 / 255.0;
            let image_color_multiplied_alpha = Rgb([
                (color.r() as f32 * alpha_f) as u8,
                (color.g() as f32 * alpha_f) as u8,
                (color.b() as f32 * alpha_f) as u8,
            ]);
            image.put_pixel(x as u32, y as u32, image_color_multiplied_alpha)
        });
        image.into()
    }

    #[instrument(skip(self), level = "TRACE")]
    pub async fn handle_command(&mut self, command: UiCommand) -> eyre::Result<()> {
        match command {
            UiCommand::Refresh => {
                for (i, button) in self
                    .page
                    .clone()
                    .into_iter()
                    .take(u8::MAX as usize)
                    .enumerate()
                {
                    let image = if let Some(r) = button.as_ref() {
                        let mut data = r.read().await;
                        self.render_button_image(&mut data).await
                    } else {
                        ImageBuffer::from_pixel(71, 71, Rgb([0u8, 0u8, 0u8])).into()
                    };

                    self.device.set_button_image(i as u8, image).await?;
                }
                self.device.flush().await?;
            }
            UiCommand::Flip(new_page) => {
                self.page = new_page;
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
                        warn!(
                            "Button {} not found at page stack depth {}",
                            key,
                            self.page.len()
                        );
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
