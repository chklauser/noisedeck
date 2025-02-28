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
    use elgato_streamdeck::asynchronous::list_devices_async;
    use elgato_streamdeck::info::Kind;
    use elgato_streamdeck::{new_hidapi, AsyncStreamDeck};
    use eyre::{Context, OptionExt};

    #[tracing::instrument]
    pub async fn run() -> Result<(), eyre::Error> {
        let mut hid = new_hidapi().context("Failed to create HIDAPI")?;
        let devices = list_devices_async(&mut hid);
        tracing::info!("Found {} devices", devices.len());
        let (kind, serial) = devices.iter().filter(|(kind,_)| *kind == Kind::Original || *kind == Kind::OriginalV2).next().ok_or_eyre("No supported StreamDeck found")?;
        
        let device = AsyncStreamDeck::connect(&hid, *kind, serial).with_context(|| format!("Failed to connect to device {:?} {}", kind, &serial))?;
        tracing::info!("Connected to '{}' with version '{}'. Key count {}", device.serial_number().await?, device.firmware_version().await?, kind.key_count());
        
        device.set_brightness(50).await?;
        device.clear_all_button_images().await?;
        
        Ok(())
    }
}
