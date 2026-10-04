//
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

//
use crate::analysis::imports::namespace_of;
use crate::analysis::source::SourceFile;
use crate::dependencies;
use crate::document::DocumentStore;
use crate::workspace::{self, WorkspaceIndex};

//
use tower_lsp::jsonrpc::Result;
use lsp_types::*;
use tower_lsp::{Client, LanguageServer};

pub struct Backend {
    client: Client,
    documents: Arc<DocumentStore>,
    /// Every package schema on disk, open or not (see `workspace`).
    workspace: Arc<WorkspaceIndex>,
    /// The workspace folders, as filesystem paths.
    roots: RwLock<Vec<PathBuf>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(DocumentStore::new()),
            workspace: Arc::new(WorkspaceIndex::default()),
            roots: RwLock::new(Vec::new()),
        }
    }

    /// The project view `uri` is analysed in, minus `uri` itself: see
    /// [`Backend::package_view`].
    fn other_project_files(&self, uri: &Url) -> Vec<SourceFile> {
        let own = workspace::path_key(uri);
        self.package_view(uri).into_iter().filter(|f| workspace::path_key(&f.uri) != own).collect()
    }

    /// Everything `uri`'s package is analysed with: its schemas (open
    /// buffers, else their disk copies), its `config.idp` (which says what
    /// its dependencies are), and its dependencies' schemas under their
    /// declared names (see `dependencies`). A file outside any package sees
    /// only the open files outside any package.
    fn package_view(&self, uri: &Url) -> Vec<SourceFile> {
        let root = workspace::package_root(uri);
        let open: Vec<crate::document::Document> = self
            .documents
            .get_all_uris()
            .into_iter()
            .filter_map(|u| self.documents.get(&u))
            .collect();
        let open_text = |url: &Url| {
            let key = workspace::path_key(url);
            open.iter().find(|d| workspace::path_key(&d.uri) == key).map(|d| d.text.clone())
        };

        let mut files: Vec<SourceFile> = open
            .iter()
            .filter(|d| !is_idp(&d.uri) && workspace::package_root(&d.uri) == root)
            .map(|d| SourceFile::local(d.uri.clone(), d.text.clone()))
            .collect();
        if root.is_none() {
            return files;
        }

        let open_keys: HashSet<Option<String>> = files.iter().map(|f| workspace::path_key(&f.uri)).collect();
        files.extend(
            self.workspace
                .package_files(uri)
                .into_iter()
                .filter(|(u, _)| !open_keys.contains(&workspace::path_key(u)))
                .map(|(u, text)| SourceFile::local(u, text)),
        );

        if let Some(manifest) = manifest_url(uri) {
            let text = open_text(&manifest).or_else(|| std::fs::read_to_string(manifest.to_file_path().ok()?).ok());
            if let Some(text) = text {
                files.push(SourceFile::local(manifest, text));
            }
        }

        for dependency in self.workspace.dependencies_of(uri) {
            for (url, text) in dependency.files {
                let mut namespace = vec![dependency.name.clone()];
                namespace.extend(namespace_of(&url));
                let text = open_text(&url).unwrap_or(text);
                files.push(SourceFile::of_dependency(url, text, &dependency.name, namespace));
            }
        }

        files
    }

    /// Re-read the dependencies of the package whose `src/` is `src_root`,
    /// from its `config.idp` buffer if open, else from disk.
    fn reindex_dependencies(&self, src_root: &std::path::Path) {
        let manifest = src_root.parent().map(|dir| dir.join(dependencies::MANIFEST));
        let open_text = manifest.and_then(|path| {
            let key = Url::from_file_path(&path).ok().and_then(|u| workspace::path_key(&u));
            self.documents
                .get_all_uris()
                .into_iter()
                .find(|u| workspace::path_key(u) == key)
                .and_then(|u| self.documents.get(&u))
                .map(|d| d.text)
        });
        self.workspace.index_dependencies(src_root, open_text.as_deref());
    }

    /// Every `.ids` file, all packages: open buffers first, then the disk
    /// copies of everything else.
    fn all_project_files(&self) -> Vec<(Url, String)> {
        let open: Vec<(Url, String)> = self
            .documents
            .get_all_uris()
            .into_iter()
            .filter(|u| !is_idp(u))
            .filter_map(|u| self.documents.get(&u).map(|d| (u, d.text)))
            .collect();
        let open_keys: HashSet<Option<String>> = open.iter().map(|(u, _)| workspace::path_key(u)).collect();

        let disk = self
            .workspace
            .all_files()
            .into_iter()
            .filter(|(u, _)| !open_keys.contains(&workspace::path_key(u)));

        open.into_iter().chain(disk).collect()
    }

    /// Index every package schema under the workspace folders, off the
    /// request loop, then re-check the open files against it.
    async fn scan_workspace(&self) {
        let roots = self.roots.read().map(|r| r.clone()).unwrap_or_default();
        let index = Arc::clone(&self.workspace);
        let scanned = tokio::task::spawn_blocking(move || {
            index.scan(&roots);
            for src_root in index.package_roots() {
                index.index_dependencies(&src_root, None);
            }
            index.len()
        })
        .await
        .unwrap_or(0);

        tracing::info!("Indexed {} schema file(s) in the workspace", scanned);
        self.publish_ids_diagnostics().await;
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

/// The `config.idp` of `uri`'s package (next to its `src/`), if it has one.
fn manifest_url(uri: &Url) -> Option<Url> {
    let path = uri.to_file_path().ok()?;
    let package_dir = comline_core::package::layout::schemas_root_for(&path)?.parent()?;
    Url::from_file_path(package_dir.join(dependencies::MANIFEST)).ok()
}

/// For a package manifest (`config.idp`), its package's `src/` directory.
fn manifest_package_src(uri: &Url) -> Option<PathBuf> {
    let path = uri.to_file_path().ok()?;
    if path.file_name()? != dependencies::MANIFEST {
        return None;
    }
    Some(path.parent()?.join(comline_core::package::layout::SCHEMAS_DIR))
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("Initializing Comline Language Server");

        #[allow(deprecated)] // `root_uri`: the fallback for clients without workspace folders
        let folders: Vec<Url> = match params.workspace_folders {
            Some(folders) => folders.into_iter().map(|f| f.uri).collect(),
            None => params.root_uri.into_iter().collect(),
        };
        if let Ok(mut roots) = self.roots.write() {
            *roots = folders.iter().filter_map(|u| u.to_file_path().ok()).collect();
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    // `:` / `(` / `>` (the back half of `->`) each mark the
                    // start of a type position; `@` marks the start of an
                    // annotation key; ` ` re-triggers a fresh request on
                    // every space so the suggestion list doesn't just
                    // vanish when a type position's `:`/`->`/`(` is
                    // followed by whitespace before the type itself (the
                    // default "trigger on word characters only" behavior
                    // otherwise closes the widget the moment a space is
                    // typed, with nothing re-opening it) — safe to request
                    // on every space since `completion::determine_context`
                    // is itself whitespace-tolerant and returns an
                    // appropriately narrow (often empty) list everywhere
                    // else.
                    trigger_characters: Some(
                        [".", ":", "(", ">", " ", "@"].iter().map(|s| s.to_string()).collect(),
                    ),
                    ..Default::default()
                }),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
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
                code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                    code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                    ..Default::default()
                })),
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

        self.scan_workspace().await;
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

        // Its buffer is gone from the project view (back to the disk copy,
        // if any), which can change what the other files' imports resolve to.
        self.client.publish_diagnostics(uri.clone(), vec![], None).await;
        if let Some(src_root) = manifest_package_src(&uri) {
            self.reindex_dependencies(&src_root);
        }
        self.publish_ids_diagnostics().await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut changed = false;
        let mut reindex: Vec<PathBuf> = Vec::new();

        for change in params.changes {
            if let Some(src_root) = manifest_package_src(&change.uri) {
                reindex.push(src_root);
                changed = true;
                continue;
            }
            if !change.uri.path().ends_with(".ids") {
                continue;
            }
            changed = true;
            if change.typ == FileChangeType::DELETED {
                self.workspace.remove(&change.uri);
            } else {
                self.workspace.refresh(&change.uri);
            }
            // A schema of some package's dependency (a sibling package, or a
            // pin `comline check` just fetched into the deps cache).
            if let Ok(path) = change.uri.to_file_path() {
                reindex.extend(self.workspace.packages_depending_on(&path));
            }
        }

        reindex.sort();
        reindex.dedup();
        for src_root in &reindex {
            self.reindex_dependencies(src_root);
        }

        if changed {
            self.publish_ids_diagnostics().await;
            self.publish_open_manifest_diagnostics().await;
        }
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        if let Ok(mut roots) = self.roots.write() {
            let removed: Vec<PathBuf> =
                params.event.removed.iter().filter_map(|f| f.uri.to_file_path().ok()).collect();
            roots.retain(|r| !removed.contains(r));
            roots.extend(params.event.added.iter().filter_map(|f| f.uri.to_file_path().ok()));
        }
        self.scan_workspace().await;
    }

    async fn symbol(&self, params: WorkspaceSymbolParams) -> Result<Option<Vec<SymbolInformation>>> {
        use crate::handlers::symbols;
        let found = symbols::get_workspace_symbols(&self.all_project_files(), &params.query);
        Ok((!found.is_empty()).then_some(found))
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

        let other_files = self.other_project_files(&uri);

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
        let completions = completion::get_completions_with_project(
            &document.text,
            &uri,
            position,
            &self.other_project_files(&uri),
        );
        
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
        
        let other_files = self.other_project_files(&uri);

        // Use our definition handler
        use crate::handlers::definition;
        Ok(definition::find_definition_with_project(
            &document.text,
            &uri,
            position,
            &other_files,
        ))
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
        let refs = references::find_references_with_project(
            &document.text,
            &uri,
            position,
            include_declaration,
            &self.other_project_files(&uri),
        );
        
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
        Ok(rename::rename_symbol_with_project(
            &document.text,
            &uri,
            position,
            &new_name,
            &self.other_project_files(&uri),
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;

        if is_idp(&uri) {
            return Ok(None);
        }

        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };

        use crate::handlers::rename;
        Ok(rename::prepare_rename_with_project(
            &document.text,
            &uri,
            params.position,
            &self.other_project_files(&uri),
        )
        .map(PrepareRenameResponse::Range))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();

        if is_idp(&uri) {
            return Ok(None);
        }

        let document = match self.documents.get(&uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };

        use crate::handlers::code_actions;
        let actions = code_actions::get_code_actions_with_project(
            &document.text,
            &uri,
            &params,
            &self.other_project_files(&uri),
        );
        Ok((!actions.is_empty()).then_some(actions))
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
            // A package manifest says what its dependencies are: what the
            // package's schemas resolve against changes with it.
            if let Some(src_root) = manifest_package_src(uri) {
                self.reindex_dependencies(&src_root);
                self.publish_ids_diagnostics().await;
            }
        } else {
            self.publish_ids_diagnostics().await;
        }
    }

    /// Re-check every open `.ids` file and publish its diagnostics. All of
    /// them, not just the one that changed: whether a name is imported, or
    /// declared somewhere at all, depends on the other files of its package
    /// too (`analysis::import_check`) - open or not. Parsed trees come from
    /// `analysis::parse_cache`, so only changed files are re-parsed.
    async fn publish_ids_diagnostics(&self) {
        use crate::analysis::diagnostics;
        use crate::analysis::project::Project;
        use crate::parser;

        let open: Vec<crate::document::Document> = self
            .documents
            .get_all_uris()
            .into_iter()
            .filter(|u| !is_idp(u))
            .filter_map(|u| self.documents.get(&u))
            .collect();

        let mut packages: BTreeMap<Option<String>, Vec<&crate::document::Document>> = BTreeMap::new();
        for document in &open {
            packages.entry(workspace::package_root(&document.uri)).or_default().push(document);
        }

        for documents in packages.values() {
            let view = self.package_view(&documents[0].uri);
            let project = Project::new(view.iter());

            for document in documents {
                // In the project exactly when it parsed cleanly.
                let lsp_diagnostics = match project.index_of(&document.uri) {
                    Some(index) => diagnostics::project_diagnostics(&project, index),
                    None => parser::parse(&document.text)
                        .map(|result| diagnostics::generate_diagnostics(&document.text, &result.errors))
                        .unwrap_or_default(),
                };

                self.client
                    .publish_diagnostics(document.uri.clone(), lsp_diagnostics, Some(document.version))
                    .await;
            }
        }
    }

    /// Re-publish every open `config.idp`'s diagnostics - whether a
    /// dependency is fetched can change without the manifest itself changing.
    async fn publish_open_manifest_diagnostics(&self) {
        for uri in self.documents.get_all_uris() {
            if is_idp(&uri) {
                self.parse_and_publish_idp_diagnostics(&uri).await;
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
                let mut lsp_diagnostics = diagnostics::generate_diagnostics(&document.text, &result.errors);
                if !result.has_errors() {
                    if let Some(package_dir) = uri.to_file_path().ok().and_then(|p| p.parent().map(PathBuf::from)) {
                        lsp_diagnostics.extend(dependencies::manifest_diagnostics(&package_dir, &document.text));
                    }
                }

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
