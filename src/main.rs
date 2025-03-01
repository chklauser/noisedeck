use clap::{Parser, Subcommand};
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Debug, Parser)]
#[command(version, about, author)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>
}

#[derive(Debug,Eq,PartialEq,Subcommand,Clone)]
enum Commands {
    Daemon
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .init();
    stable_eyre::install()?;

    let cli = Cli::parse();
    tracing::debug!("Parsed command line arguments {:?}", &cli);

    match &cli.command {
        Some(Commands::Daemon) => {
            daemon::run().await?;
        },
        None => {
            return Ok(());
        }
    }

    Ok(())
}

mod daemon {
    use std::iter::repeat;
    use cosmic_text::{Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, Weight};
    use elgato_streamdeck::asynchronous::list_devices_async;
    use elgato_streamdeck::info::Kind;
    use elgato_streamdeck::{new_hidapi, AsyncStreamDeck, DeviceStateUpdate};
    use eyre::{Context, OptionExt, Report};
    use image::{ImageBuffer, Rgb};
    use imageproc::image::RgbImage;
    use tracing::{debug, info, warn};

    #[tracing::instrument]
    pub async fn run() -> Result<(), eyre::Error> {
        let mut hid = new_hidapi().context("Failed to create HIDAPI")?;
        let devices = list_devices_async(&mut hid);
        info!("Found {} devices", devices.len());
        let (kind, serial) = devices.iter().filter(|(kind,_)| *kind == Kind::Original || *kind == Kind::OriginalV2).next().ok_or_eyre("No supported StreamDeck found")?;

        let device = AsyncStreamDeck::connect(&hid, *kind, serial).with_context(|| format!("Failed to connect to device {:?} {}", kind, &serial))?;
        debug!("Connected to '{}' with version '{}'. Key count {}", device.serial_number().await?, device.firmware_version().await?, kind.key_count());

        device.set_brightness(60).await?;
        device.clear_all_button_images().await?;

        let mut font_system = load_fonts().await?;
        let mut swash_cache = SwashCache::new();

        for i in 0..kind.key_count() {
            let text = format!("Btn {i}");
            let image = render_button_image(&mut font_system, &mut swash_cache, &text);
            device.set_button_image(i, image.into()).await?;
        }
        device.flush().await?;

        let mut state = repeat(0u16).take(device.kind().key_count() as usize).collect::<Vec<_>>();
        let reader = device.get_reader();
        'infinite: loop {
            let updates = reader.read(100.0).await.context("Failed to read updates")?;

            match handle_updates(&device, &mut font_system, &mut swash_cache, &mut state, updates).await {
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "Error handling updates");
                    break 'infinite;
                }
            }
        }
        drop(reader);

        Ok(())
    }

    #[tracing::instrument(level = "TRACE", skip_all)]
    async fn handle_updates(device: &AsyncStreamDeck, mut font_system: &mut FontSystem, mut swash_cache: &mut SwashCache, state: &mut Vec<u16>, updates: Vec<DeviceStateUpdate>) -> Result<(), Report> {
        let mut overall_flush_needed = false;
        for update in updates {
            overall_flush_needed |= match update {
                DeviceStateUpdate::ButtonDown(key) => {
                    info!("Button {} down", key);
                    false
                },
                DeviceStateUpdate::ButtonUp(key) => {
                    info!("Button {} up", key);
                    state[key as usize] += 1;
                    device.set_button_image(key, render_button_image(&mut font_system, &mut swash_cache,
                        &format!("Btn {}\n{}", key, state[key as usize])).into()).await?;
                    true
                },
                unknown => {
                    info!("Ignoring device update: {:?}", unknown);
                    false
                }
            };
        }

        if overall_flush_needed {
            device.flush().await?;
        }
        Ok(())
    }

    #[tracing::instrument(level = "TRACE", skip(font_system, swash_cache))]
    fn render_button_image(mut font_system: &mut FontSystem, mut swash_cache: &mut SwashCache, text: &str) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        let mut image = RgbImage::from_pixel(71, 71, Rgb([0u8, 0u8, 0u8]));
        let metrics = Metrics::new(16.0, 24.0);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        let mut buffer = buffer.borrow_with(&mut font_system);
        buffer.set_size(Some(72.0), Some(72.0));
        let mut attrs = Attrs::new();
        attrs.weight = Weight::EXTRA_BOLD;
        buffer.set_text(text, attrs, Shaping::Advanced);
        //buffer.set_text("Hello World, fine däy, eh?", attrs, Shaping::Advanced);
        buffer.shape_until_scroll(true);
        let text_color = cosmic_text::Color::rgb(0xFF, 0xFF, 0xFF);
        buffer.draw(&mut swash_cache, text_color, |x, y, _w, _h, color| {
            if x < 0 || y < 0 || x >= 71 || y >= 71 {
                if x < -1 || y < -1 || x > 71 || y > 71 {
                    warn!("Out of bounds: x: {}, y: {}", x, y);
                }
                return;
            }
            let alpha_f = color.a() as f32 / 255.0;
            let image_color_multiplied_alpha = Rgb([(color.r() as f32 * alpha_f) as u8, (color.g() as f32 * alpha_f) as u8, (color.b() as f32 * alpha_f) as u8]);
            image.put_pixel(x as u32, y as u32, image_color_multiplied_alpha)
        });
        image
    }

    #[tracing::instrument(level = tracing::Level::DEBUG)]
    async fn load_fonts() -> eyre::Result<FontSystem> {
        let emoji_font_data = Vec::from(include_bytes!("../font/noto-color-emoji/NotoColorEmoji-NoSvg.ttf"));
        let sans_font_data = Vec::from(include_bytes!("../font/noto-sans/static/NotoSans-Medium.ttf"));
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
        }).await.context("Failed to load fonts")
    }
}
