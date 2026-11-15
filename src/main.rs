//! The `comline-lsp` binary — a `tower-lsp` stdio server over the analysis
//! library. Built only with the `server` feature (see Cargo.toml).

use comline_language_server::backend::Backend;
use comline_language_server::version;
use tower_lsp::{LspService, Server};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("comline-lsp {}", version::server_version());
        return;
    }

    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    tracing::info!("Starting Comline Language Server");

    let (service, socket) = LspService::build(Backend::new)
        .custom_method("comline/stdSource", Backend::std_source)
        .finish();
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;

    tracing::info!("Comline Language Server stopped");
}
