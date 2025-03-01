use crate::daemon::ui::{ButtonBehavior, ButtonData, ButtonRef, Command};
use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, Weight};
use elgato_streamdeck::asynchronous::list_devices_async;
use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate, new_hidapi};
use eyre::{Context, OptionExt, Report};
use image::{DynamicImage, ImageBuffer, Rgb};
use imageproc::image::RgbImage;
use tracing::{debug, error, info, instrument, warn};

mod ui;

#[tracing::instrument]
pub async fn run() -> Result<(), eyre::Error> {
    let mut hid = new_hidapi().context("Failed to create HIDAPI")?;
    let devices = list_devices_async(&mut hid);
    info!("Found {} devices", devices.len());
    let (kind, serial) = devices
        .iter()
        .filter(|(kind, _)| *kind == Kind::Original || *kind == Kind::OriginalV2)
        .next()
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

    let (mut deck, event_tx, mut command_rx) = ui::NoiseDeck::new(device.kind());
    deck.push_page(
        (0..kind.key_count())
            .map(|i| {
                Some(
                    ui::Button::builder()
                        .data(ButtonData {
                            label: format!("Btn {i}").into(),
                        })
                        .on_tap(ButtonBehavior::Increment(i))
                        .build()
                        .into(),
                )
            })
            .collect(),
    )
    .await?;
    let deck_finished = tokio::spawn(deck.run());

    let font_system = load_fonts().await?;
    let swash_cache = SwashCache::new();
    let mut state = DeckState {
        page_stack: vec![vec![]],
        font_system,
        swash_cache,
        device,
        event_tx,
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
            command = command_rx.recv() => {
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
    deck_finished.await??;

    if device.shutdown().await.is_err() {
        if device.sleep().await.is_err() {
            device.set_brightness(15).await?;
        }
    }

    Ok(())
}

struct DeckState {
    page_stack: Vec<Vec<Option<ButtonRef>>>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    device: AsyncStreamDeck,
    event_tx: tokio::sync::mpsc::Sender<ui::Event>,
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
        buffer.set_text(&button.label, attrs, Shaping::Advanced);
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
    pub async fn handle_command(&mut self, command: Command) -> eyre::Result<()> {
        match command {
            Command::Refresh => {
                for (i, button) in self
                    .current_page()?
                    .iter()
                    .take(u8::MAX as usize)
                    .enumerate()
                    .map(|(i, b)| (i, b.clone()))
                {
                    let image = if let Some(r) = button.as_ref() {
                        let mut data = r.read().await;
                        self.render_button_image(&mut data).await.into()
                    } else {
                        ImageBuffer::from_pixel(71, 71, Rgb([0u8, 0u8, 0u8])).into()
                    };

                    self.device.set_button_image(i as u8, image).await?;
                }
                self.device.flush().await?;
            }
            Command::PushPage(new_page) => {
                self.page_stack.push(new_page);
                Box::pin(self.handle_command(Command::Refresh)).await?;
            }
            Command::PopPage => {
                if self.page_stack.len() > 1 {
                    self.page_stack.pop();
                } else {
                    error!("Attempted to pop last page");
                }
            }
        }
        Ok(())
    }

    fn current_page(&mut self) -> eyre::Result<Vec<Option<ButtonRef>>> {
        self.page_stack
            .last()
            .ok_or_eyre("Empty page stack")
            .map(|p| p.clone())
    }

    fn button_by_key(&mut self, key: u8) -> eyre::Result<Option<ButtonRef>> {
        Ok(self
            .page_stack
            .last()
            .ok_or_eyre("Empty page stack")?
            .get(key as usize)
            .and_then(|b| b.clone()))
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
                        self.event_tx.send(ui::Event::ButtonTap(button)).await?;
                    } else {
                        warn!(
                            "Button {} not found at page stack depth {}",
                            key,
                            self.page_stack.len()
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
