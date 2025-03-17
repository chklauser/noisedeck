#![allow(dead_code)]

use crate::import::ImportArgs;
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Debug, Parser)]
#[command(version, about, author)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Eq, PartialEq, Subcommand, Clone)]
enum Commands {
    Daemon,
    Import(ImportArgs),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let no_env_var_file= dotenv();
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .init();
    stable_eyre::install()?;
    if let Err(e) = no_env_var_file {
        if e.not_found() {
            tracing::debug!("No .env file found");
        } else {
            tracing::debug!("Failed to load .env file: {}", e);
        }
    }

    let cli = Cli::parse();
    tracing::debug!("Parsed command line arguments {:?}", &cli);

    match &cli.command {
        Some(Commands::Daemon) => {
            daemon::run().await?;
        }
        Some(Commands::Import(args)) => {
            import::run(args).await?;
        }
        None => {
            return Ok(());
        }
    }

    Ok(())
}

mod daemon;
mod import;

mod config {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use std::sync::Arc;
    use uuid::Uuid;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Config {
        pub pages: HashMap<Uuid, Arc<Page>>,
        pub start_page: Uuid,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Page {
        pub name: String,
        pub buttons: Vec<Button>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Button {
        pub label: Arc<String>,
        pub behavior: ButtonBehavior,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub enum ButtonBehavior {
        PushPage(Uuid),
        PlaySound { path: Arc<String> },
    }
}
