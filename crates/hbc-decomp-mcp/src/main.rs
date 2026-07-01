use std::sync::Arc;

use clap::Parser;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServiceExt;
use tokio::io::{stdin, stdout};

mod server;

use server::HermesService;

/// Hermes bytecode decompiler — MCP server.
///
/// By default it speaks the MCP protocol over stdio (for Claude Desktop/Code,
/// Cursor, …). Pass `--transport http` to instead serve over Streamable HTTP,
/// which lets multiple/remote clients connect to a single running instance.
#[derive(Parser, Debug)]
#[command(name = "hermes-mcp", version, about)]
struct Args {
    /// Transport to serve on: `stdio` (default) or `http` (Streamable HTTP).
    #[arg(long, default_value = "stdio")]
    transport: Transport,

    /// Host/interface to bind when `--transport http` (default: loopback only).
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// TCP port to bind when `--transport http`.
    #[arg(long, default_value_t = 8744)]
    port: u16,

    /// HTTP path the MCP endpoint is mounted at (only for `--transport http`).
    #[arg(long, default_value = "/mcp")]
    path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Transport {
    Stdio,
    Http,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    match args.transport {
        Transport::Stdio => {
            // One session bound to this process's stdin/stdout.
            let service = HermesService::new();
            let server = service.serve((stdin(), stdout())).await?;
            server.waiting().await?;
        }
        Transport::Http => {
            // Streamable HTTP: each client session gets its own HermesService
            // (its own loaded file), created on demand by the factory.
            let service = StreamableHttpService::new(
                || Ok(HermesService::new()),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default(),
            );
            let router = axum::Router::new().nest_service(&args.path, service);

            let addr = format!("{}:{}", args.host, args.port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            // Log to stderr so it never pollutes an MCP stdout channel.
            eprintln!("hermes-mcp: Streamable HTTP on http://{addr}{}", args.path);
            axum::serve(listener, router).await?;
        }
    }

    Ok(())
}
