//! Comline language server.
//!
//! The **analysis** layer — `parser`, `analysis`, `handlers`, `util` — is
//! always built and depends only on `lsp-types` + `comline-core`, so it
//! compiles for `wasm32-unknown-unknown`. The Comline playground links the
//! crate with `default-features = false` and calls the same handlers the LSP
//! does.
//!
//! The `server` feature (on by default) adds `document` (the doc store) and
//! `backend` (the `tower-lsp` `LanguageServer` impl behind the `comline-lsp`
//! binary).

pub mod parser;
pub mod util;

pub mod analysis {
    pub mod diagnostics;
    pub mod imports;
    pub mod symbols;
    pub mod types;
}

pub mod handlers {
    pub mod code_actions;
    pub mod completion;
    pub mod definition;
    pub mod formatting;
    pub mod hover;
    pub mod references;
    pub mod rename;
    pub mod semantic_tokens;
    pub mod signature_help;
    pub mod symbols;
}

#[cfg(feature = "server")]
pub mod backend;
#[cfg(feature = "server")]
pub mod document;
