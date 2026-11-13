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

pub mod analysis;
pub mod handlers;

#[cfg(test)]
mod test_support;

#[cfg(feature = "server")]
pub mod acceptance;
#[cfg(feature = "server")]
pub mod backend;
#[cfg(feature = "server")]
pub mod dependencies;
#[cfg(feature = "server")]
pub mod document;
#[cfg(feature = "server")]
pub mod workspace;
