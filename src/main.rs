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

mod daemon;

