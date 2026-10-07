//! Comline language server.
//!
//! The **analysis** layer — `parser`, `analysis`, `handlers`, `util` — is
//! always built and depends only on `lsp-types` + `comline-core`, so it
//! compiles for `wasm32-unknown-unknown`. The Comline playground links the
//! crate with `default-features = false` and calls the same handlers the LSP
//! does.
//!
//! The `server` feature (on by default) adds `document` (the doc store),
//! `workspace` (the index of every package schema on disk) and `backend`
//! (the `tower-lsp` `LanguageServer` impl behind the `comline-lsp` binary).

pub mod parser;
pub mod util;

pub mod analysis {
    pub mod diagnostics;
    pub mod import_check;
    pub mod imports;
    pub mod modules;
    pub mod parse_cache;
    pub mod project;
    pub mod source;
    pub mod stdlib;
    pub mod symbols;
}

pub mod handlers {
    pub mod annotation_hover;
    pub mod code_actions;
    pub mod completion;
    pub mod definition;
    pub mod formatting;
    pub mod hover;
    pub mod idp_hover;
    pub mod references;
    pub mod rename;
    pub mod semantic_tokens;
    pub mod signature_help;
    pub mod symbols;
    pub mod unit_hover;
}

#[cfg(feature = "server")]
pub mod backend;
#[cfg(feature = "server")]
pub mod dependencies;
#[cfg(feature = "server")]
pub mod document;
#[cfg(feature = "server")]
pub mod workspace;
