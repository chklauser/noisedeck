use clap::{Parser, Subcommand};

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
    tracing_subscriber::fmt::init();
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
    use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
    use elgato_streamdeck::asynchronous::list_devices_async;
    use elgato_streamdeck::info::Kind;
    use elgato_streamdeck::{new_hidapi, AsyncStreamDeck};
    use eyre::{Context, OptionExt};
    use image::{DynamicImage, Pixel, Rgb, Rgba, RgbaImage};
    use imageproc::drawing::Canvas;
    use imageproc::image::RgbImage;
    use std::sync::Arc;
    use image::ColorType::Rgb8;
    use tracing::{info, warn};

    #[tracing::instrument]
    pub async fn run() -> Result<(), eyre::Error> {
        let mut hid = new_hidapi().context("Failed to create HIDAPI")?;
        let devices = list_devices_async(&mut hid);
        tracing::info!("Found {} devices", devices.len());
        let (kind, serial) = devices.iter().filter(|(kind,_)| *kind == Kind::Original || *kind == Kind::OriginalV2).next().ok_or_eyre("No supported StreamDeck found")?;

        let device = AsyncStreamDeck::connect(&hid, *kind, serial).with_context(|| format!("Failed to connect to device {:?} {}", kind, &serial))?;
        tracing::info!("Connected to '{}' with version '{}'. Key count {}", device.serial_number().await?, device.firmware_version().await?, kind.key_count());

        device.set_brightness(60).await?;
        device.clear_all_button_images().await?;

        let mut image = RgbImage::from_pixel(71, 71, Rgb([0u8,0u8,0u8]));
        let mut font_system = load_fonts().await?;
        let mut swash_cache = SwashCache::new();
        let metrics = Metrics::new(12.0, 20.0);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        let mut buffer = buffer.borrow_with(&mut font_system);
        buffer.set_size(Some(72.0), Some(72.0));
        let mut attrs = Attrs::new();
        buffer.set_text("🦄Hello World, fine day?🧛‍♂️🪵", attrs, Shaping::Advanced);
        //buffer.set_text("Hello World, fine däy, eh?", attrs, Shaping::Advanced);
        buffer.shape_until_scroll(true);
        let text_color = cosmic_text::Color::rgb(0xFF, 0xFF, 0xFF);
        buffer.draw(&mut swash_cache, text_color, |x, y, _w, _h, color|{
            if x < 0 || y < 0 || x >= 71 || y >= 71 {
                if x < -1 || y < -1 || x > 71 || y > 71 {
                    warn!("Out of bounds: x: {}, y: {}", x, y);
                }
                return;
            }
            let image_color_alpha = Rgba([color.r(), color.g(), color.b(), color.a()]);
            let alpha_f = color.a() as f32 / 255.0;
            let image_color_multiplied_alpha = Rgb([(color.r() as f32 * alpha_f) as u8, (color.g() as f32 * alpha_f) as u8, (color.b() as f32 * alpha_f) as u8]);
            image.put_pixel(x as u32, y as u32, image_color_multiplied_alpha)
        });

        info!("Key image format {:?}",kind.key_image_format());
        let image = DynamicImage::from(image);

        for i in 0..kind.key_count() {
            device.set_button_image(i, image.clone()).await?;
        }
        device.flush().await?;

        Ok(())
    }

    #[tracing::instrument(level = "debug")]
    async fn load_fonts() -> eyre::Result<FontSystem> {
        let emoji_font_data = Vec::from(include_bytes!("../font/noto-color-emoji/NotoColorEmoji-NoSvg.ttf"));
        let sans_font_data = Vec::from(include_bytes!("../font/noto-sans/static/NotoSans-Medium.ttf"));
        tokio::task::spawn_blocking(move || {
            // FontSystem::new_with_fonts(fonts)
            // FontSystem::new()
            let mut db = cosmic_text::fontdb::Database::new();
            db.load_font_data(emoji_font_data);
            db.load_font_data(sans_font_data);
            db.set_sans_serif_family("Noto Sans".to_owned());
            db.set_serif_family("Noto Sans".to_owned());
            db.set_monospace_family("Noto Sans".to_owned());
            db.set_cursive_family("Noto Sans".to_owned());
            db.set_fantasy_family("Noto Sans".to_owned());
            FontSystem::new_with_locale_and_db("en-US".to_owned(), db)
        }).await.context("Failed to load fonts")
    }
}
