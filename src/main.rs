use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

use postal_mcp::admin::cli::AdminCommand;
use postal_mcp::admin::login::{self, LoginRequest};
use postal_mcp::admin::{Admin, DEFAULT_CLIENT, InteractiveTerminal, cli};
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
    /// Connect a provider account and let an AI client use it, in one
    /// guided step. Asks its questions on the terminal.
    Login {
        /// Provider id, e.g. "lob". Asked for when omitted.
        provider: Option<String>,
        /// Grant to this `serve --client` name only, to keep AI clients
        /// apart. Not needed with a single client.
        #[arg(long)]
        client: Option<String>,
    },
    /// Disconnect a provider account and delete its stored credential.
    Logout {
        /// Needed only when more than one account is connected.
        connection_id: Option<String>,
    },
    /// Manage connections and permissions. Not available to the AI.
    Admin {
        #[command(subcommand)]
        command: Box<AdminCommand>,
    },
}

fn admin<'a>(
    store: &'a Store,
    providers: &'a Registry,
    terminal: &'a mut InteractiveTerminal,
) -> Admin<'a> {
    Admin {
        store,
        providers,
        secrets: &KeyringStore,
        terminal,
    }
}

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
        Command::Login { provider, client } => {
            let outcome = admin(&store, &providers, &mut InteractiveTerminal)
                .login(LoginRequest { provider, client })
                .await?;
            println!("{}", login::describe_outcome(&outcome));
        }
        Command::Logout { connection_id } => {
            let mut terminal = InteractiveTerminal;
            let mut admin = admin(&store, &providers, &mut terminal);
            let connected: Vec<_> = admin
                .connections()?
                .into_iter()
                .filter(|c| c.is_ready())
                .collect();
            let id = match (connection_id, connected.as_slice()) {
                (Some(id), _) => id,
                (None, [only]) => only.id.clone(),
                (None, []) => anyhow::bail!("no account is connected"),
                (None, several) => {
                    let ids: Vec<&str> = several.iter().map(|c| c.id.as_str()).collect();
                    anyhow::bail!(
                        "several accounts are connected; name one: {}",
                        ids.join(", ")
                    )
                }
            };
            admin.disconnect(&id)?;
            println!("Disconnected: {id}");
        }
        Command::Admin { command } => {
            let mut terminal = InteractiveTerminal;
            let mut admin = admin(&store, &providers, &mut terminal);
            cli::run(*command, &mut admin, &mut std::io::stdout()).await?;
        }
    }
    Ok(())
}
