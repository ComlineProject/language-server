//
use std::sync::Arc;

//
use crate::document::DocumentStore;

//
use tower_lsp::jsonrpc::Result;
use lsp_types::*;
use tower_lsp::{Client, LanguageServer};

pub struct Backend {
    client: Client,
    documents: Arc<DocumentStore>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(DocumentStore::new()),
        }
    }
}

/// Whether `uri` is a `.idp` (package/congregation config) document, as
/// opposed to a `.ids` schema. `DocumentStore` carries no language id (just
/// `{uri, version, text}`), so this is a plain extension check wherever it
/// matters — every handler below except diagnostics is `.ids`-only (built
/// against `comline_core::schema::idl::grammar::Document`) and must not run
/// on `.idp` text, which the client's completion/hover providers already
/// handle correctly on their own (see `comline-vscode`'s `idpSchema.ts`).
fn is_idp(uri: &Url) -> bool {
    uri.path().ends_with(".idp")
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("Initializing Comline Language Server");

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    // `:` / `(` / `>` (the back half of `->`) each mark the
                    // start of a type position; ` ` re-triggers a fresh
                    // request on every space so the suggestion list doesn't
                    // just vanish when a type position's `:`/`->`/`(` is
                    // followed by whitespace before the type itself (the
                    // default "trigger on word characters only" behavior
                    // otherwise closes the widget the moment a space is
                    // typed, with nothing re-opening it) — safe to request
                    // on every space since `completion::determine_context`
                    // is itself whitespace-tolerant and returns an
                    // appropriately narrow (often empty) list everywhere
                    // else.
                    trigger_characters: Some(
                        [".", ":", "(", ">", " "].iter().map(|s| s.to_string()).collect(),
                    ),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                document_formatting_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                // Order must match the indices in
                                // `handlers::semantic_tokens` (`LEGEND_TYPES`).
                                token_types: vec![
                                    SemanticTokenType::KEYWORD,
                                    SemanticTokenType::TYPE,
                                    SemanticTokenType::STRING,
                                    SemanticTokenType::COMMENT,
                                    SemanticTokenType::NUMBER,
                                    SemanticTokenType::DECORATOR,
                                ],
                                token_modifiers: vec![],
                            },
                            range: Some(false),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            ..Default::default()
                        },
                    ),
                ),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "Comline Language Server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        tracing::info!("Comline Language Server initialized");
        self.client
            .log_message(MessageType::INFO, "Comline LSP ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        tracing::info!("Shutting down Comline Language Server");
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        tracing::debug!("Document opened: {}", uri);
        self.documents.insert(uri.clone(), version, text);

        // Parse and send diagnostics
        self.parse_and_publish_diagnostics(&uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().next() {
            tracing::debug!("Document changed: {}", uri);
            self.documents.update(&uri, version, change.text);

            // Re-parse and send diagnostics
            self.parse_and_publish_diagnostics(&uri).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        tracing::debug!("Document closed: {}", uri);
        self.documents.remove(&uri);
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        if is_idp(&uri) {
            return Ok(None); // handled client-side — see idpSchema.ts
        }

        tracing::debug!("Hover request for {} at {:?}", uri, position);

        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };

        // Every other file currently open as a buffer, so a cross-file type
        // reference can still resolve (only covers open buffers, not the
        // whole workspace — there's no workspace scan on `initialize`).
        let other_files: Vec<(Url, String)> = self
            .documents
            .get_all_uris()
            .into_iter()
            .filter(|u| u != &uri)
            .filter_map(|u| self.documents.get(&u).map(|d| (u, d.text)))
            .collect();

        // Use our hover handler
        use crate::handlers::hover;
        Ok(hover::get_hover_info_with_project(
            &document.text,
            &uri,
            position,
            &other_files,
        ))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        if is_idp(&uri) {
            return Ok(None); // handled client-side — see idpSchema.ts
        }

        tracing::debug!("Completion request for {} at {:?}", uri, position);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our completion handler
        use crate::handlers::completion;
        let completions = completion::get_completions(&document.text, &uri, position);
        
        if completions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResponse::Array(completions)))
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        if is_idp(&uri) {
            return Ok(None);
        }

        tracing::debug!("Go-to-definition request for {} at {:?}", uri, position);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our definition handler
        use crate::handlers::definition;
        Ok(definition::find_definition(&document.text, &uri, position))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        if is_idp(&uri) {
            return Ok(None);
        }

        tracing::debug!("Find references request for {} at {:?}", uri, position);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our references handler
        use crate::handlers::references;
        let refs = references::find_references(&document.text, &uri, position, include_declaration);
        
        if refs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(refs))
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        if is_idp(&uri) {
            return Ok(None);
        }

        tracing::debug!("Document symbols request for {}", uri);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our symbols handler
        use crate::handlers::symbols;
        let doc_symbols = symbols::get_document_symbols(&document.text, &uri);
        
        if doc_symbols.is_empty() {
            Ok(None)
        } else {
            Ok(Some(DocumentSymbolResponse::Nested(doc_symbols)))
        }
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;

        // No `is_idp` guard here, deliberately unlike every other handler —
        // `handlers::formatting`'s brace-depth indentation pass is already
        // fully language-agnostic (plain line/brace text processing, no
        // dependency on `.ids`'s grammar types at all), and `.idp` nests
        // with `{}` the same way `.ids` does, so it applies correctly as-is.
        tracing::debug!("Format request for {}", uri);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our formatting handler
        use crate::handlers::formatting;
        let edits = formatting::format_document(&document.text);
        
        if edits.is_empty() {
            Ok(None)
        } else {
            Ok(Some(edits))
        }
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;

        if is_idp(&uri) {
            return Ok(None);
        }

        tracing::debug!("Rename request for {} at {:?} to '{}'", uri, position, new_name);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our rename handler
        use crate::handlers::rename;
        Ok(rename::rename_symbol(&document.text, &uri, position, &new_name))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;

        if is_idp(&uri) {
            return Ok(None);
        }

        tracing::debug!("Semantic tokens request for {}", uri);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our semantic tokens handler directly
        Ok(crate::handlers::semantic_tokens::get_semantic_tokens(&document.text, &uri))
    }
}

impl Backend {
    /// Parse a document and publish diagnostics — `.idp` and `.ids` take
    /// different paths (different grammars, and `.idp` gets parse-error
    /// diagnostics only, no semantic validation pass — see
    /// `parser::IdpParseResult`'s doc comment for why).
    async fn parse_and_publish_diagnostics(&self, uri: &Url) {
        if is_idp(uri) {
            self.parse_and_publish_idp_diagnostics(uri).await;
        } else {
            self.parse_and_publish_ids_diagnostics(uri).await;
        }
    }

    async fn parse_and_publish_ids_diagnostics(&self, uri: &Url) {
        use crate::analysis::diagnostics;
        use crate::parser;

        let document = match self.documents.get(uri) {
            Some(doc) => doc,
            None => return,
        };

        // Parse the document
        match parser::parse(&document.text) {
            Ok(result) => {
                // Parse-error diagnostics, plus `comline-core`'s validation
                // pass once the tree is well-formed.
                let lsp_diagnostics = diagnostics::all_diagnostics(
                    &document.text,
                    &result.errors,
                    result.document.as_ref(),
                );

                // Log parse results
                if result.is_ok() {
                    if let Some(doc) = &result.document {
                        tracing::debug!("Successfully parsed {}: {} declarations", uri, parser::get_declaration_count(doc));
                    }
                } else {
                    tracing::debug!("Parse errors for {}: {} error(s)", uri, result.errors.len());
                }

                // Publish diagnostics to client
                self.client
                    .publish_diagnostics(uri.clone(), lsp_diagnostics, Some(document.version))
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to parse {}: {}", uri, e);
                // Clear diagnostics on internal error
                self.client
                    .publish_diagnostics(uri.clone(), vec![], Some(document.version))
                    .await;
            }
        }
    }

    async fn parse_and_publish_idp_diagnostics(&self, uri: &Url) {
        use crate::analysis::diagnostics;
        use crate::parser;

        let document = match self.documents.get(uri) {
            Some(doc) => doc,
            None => return,
        };

        match parser::parse_idp(&document.text) {
            Ok(result) => {
                let lsp_diagnostics = diagnostics::generate_diagnostics(&document.text, &result.errors);

                if result.has_errors() {
                    tracing::debug!(".idp parse errors for {}: {} error(s)", uri, result.errors.len());
                } else {
                    tracing::debug!("Successfully parsed .idp {}", uri);
                }

                self.client
                    .publish_diagnostics(uri.clone(), lsp_diagnostics, Some(document.version))
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to parse .idp {}: {}", uri, e);
                self.client
                    .publish_diagnostics(uri.clone(), vec![], Some(document.version))
                    .await;
            }
        }
    }
}
