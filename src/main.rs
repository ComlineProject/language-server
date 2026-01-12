use tower_lsp::{LspService, Server};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod backend;
mod document;
mod parser;
mod util;

mod analysis {
    pub mod diagnostics;
    pub mod imports;
    pub mod symbols;
    pub mod types;
}

mod handlers {
    pub mod completion;
    pub mod definition;
    pub mod formatting;
    pub mod hover;
    pub mod references;
    pub mod rename;
    pub mod symbols;
}

use backend::Backend;

#[tokio::main]
async fn main() {
    // Initialize tracing for logging
    tracing_subscriber::registry()
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    tracing::info!("Starting Comline Language Server");

    // Create the LSP service
    let (service, socket) = LspService::new(|client| Backend::new(client));

    // Start the server using stdio
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;

    tracing::info!("Comline Language Server stopped");
}
