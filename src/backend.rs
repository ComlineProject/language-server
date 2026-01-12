use crate::document::DocumentStore;
use std::sync::Arc;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
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
                    trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
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
                                token_types: vec![
                                    SemanticTokenType::KEYWORD,
                                    SemanticTokenType::TYPE,
                                    SemanticTokenType::STRUCT,
                                    SemanticTokenType::ENUM,
                                    SemanticTokenType::INTERFACE,
                                    SemanticTokenType::FUNCTION,
                                    SemanticTokenType::VARIABLE,
                                    SemanticTokenType::PROPERTY,
                                ],
                                token_modifiers: vec![SemanticTokenModifier::DECLARATION],
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
        
        tracing::debug!("Hover request for {} at {:?}", uri, position);
        
        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        
        // Use our hover handler
        use crate::handlers::hover;
        Ok(hover::get_hover_info(&document.text, &uri, position))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        tracing::debug!("Completion request at {:?}", params.text_document_position);
        // TODO: Implement completion
        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        
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
        tracing::debug!("Format request for {}", params.text_document.uri);
        // TODO: Implement formatting
        Ok(None)
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        tracing::debug!("Rename request at {:?}", params.text_document_position);
        // TODO: Implement rename
        Ok(None)
    }
}

impl Backend {
    /// Parse a document and publish diagnostics
    async fn parse_and_publish_diagnostics(&self, uri: &Url) {
        use crate::analysis::diagnostics;
        use crate::parser;
        
        let document = match self.documents.get(uri) {
            Some(doc) => doc,
            None => return,
        };

        // Parse the document
        match parser::parse(&document.text) {
            Ok(result) => {
                // Generate LSP diagnostics from parse errors
                let lsp_diagnostics = diagnostics::generate_diagnostics(&document.text, &result.errors);
                
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
}
