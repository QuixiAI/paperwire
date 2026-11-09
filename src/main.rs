use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

use postal_mcp::{mcp::PostalServer, providers::Registry};

#[derive(Parser)]
#[command(version, about = "Universal Correspondence MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server over stdio (the default).
    Serve,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout carries MCP messages; logs go to stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("POSTAL_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let service = PostalServer::new(Registry::builtin())
                .serve(stdio())
                .await?;
            service.waiting().await?;
        }
    }
    Ok(())
}
