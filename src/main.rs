use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

use postal_mcp::admin::cli::AdminCommand;
use postal_mcp::admin::{Admin, InteractiveTerminal, cli};
use postal_mcp::connections::KeyringStore;
use postal_mcp::documents::DocumentDirs;
use postal_mcp::mcp::PostalServer;
use postal_mcp::providers::Registry;
use postal_mcp::store::{Store, default_data_dir};

#[derive(Parser)]
#[command(version, about = "Universal Correspondence MCP server")]
struct Cli {
    /// Where the database lives. Defaults to ~/.local/share/postal-mcp.
    #[arg(long, global = true, env = "POSTAL_MCP_DATA_DIR", value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server over stdio (the default).
    Serve {
        /// The name of the AI client this server instance serves. Grants are
        /// made to this name with `admin grant --client`.
        #[arg(long, default_value = DEFAULT_CLIENT)]
        client: String,
        /// A directory the AI client may import PDF files from. Repeatable.
        /// Without one, no file is ever read.
        #[arg(long = "document-dir", value_name = "DIR")]
        document_dirs: Vec<PathBuf>,
    },
    /// Manage connections and permissions. Not available to the AI.
    Admin {
        #[command(subcommand)]
        command: Box<AdminCommand>,
    },
}

const DEFAULT_CLIENT: &str = "default";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // In `serve`, stdout carries MCP messages; logs go to stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("POSTAL_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let data_dir = cli
        .data_dir
        .or_else(default_data_dir)
        .context("no data directory: set --data-dir or HOME")?;
    let store = Store::open(&data_dir)
        .with_context(|| format!("opening the database in {}", data_dir.display()))?;
    let providers = Registry::builtin().context("starting the HTTP client")?;

    let command = cli.command.unwrap_or(Command::Serve {
        client: DEFAULT_CLIENT.into(),
        document_dirs: Vec::new(),
    });
    match command {
        Command::Serve {
            client,
            document_dirs,
        } => {
            let document_dirs =
                DocumentDirs::new(&document_dirs).context("opening a --document-dir")?;
            let secrets = Arc::new(KeyringStore);
            let service = PostalServer::new(providers, store, secrets, document_dirs, client)
                .serve(stdio())
                .await?;
            service.waiting().await?;
        }
        Command::Admin { command } => {
            let mut admin = Admin {
                store: &store,
                providers: &providers,
                secrets: &KeyringStore,
                terminal: &mut InteractiveTerminal,
            };
            cli::run(*command, &mut admin, &mut std::io::stdout()).await?;
        }
    }
    Ok(())
}
