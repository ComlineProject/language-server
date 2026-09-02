use dashmap::DashMap;
use std::sync::Arc;
use lsp_types::Url;

/// Represents a document in the workspace
#[derive(Debug, Clone)]
pub struct Document {
    /// The URI of the document
    pub uri: Url,
    /// The version number of the document
    pub version: i32,
    /// The text content of the document
    pub text: String,
}

impl Document {
    pub fn new(uri: Url, version: i32, text: String) -> Self {
        Self { uri, version, text }
    }

    pub fn update(&mut self, version: i32, text: String) {
        self.version = version;
        self.text = text;
    }
}

/// Thread-safe document store
pub struct DocumentStore {
    documents: Arc<DashMap<Url, Document>>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            documents: Arc::new(DashMap::new()),
        }
    }

    /// Insert or update a document
    pub fn insert(&self, uri: Url, version: i32, text: String) {
        let document = Document::new(uri.clone(), version, text);
        self.documents.insert(uri, document);
    }

    /// Update an existing document
    pub fn update(&self, uri: &Url, version: i32, text: String) {
        if let Some(mut doc) = self.documents.get_mut(uri) {
            doc.update(version, text);
        }
    }

    /// Remove a document
    pub fn remove(&self, uri: &Url) {
        self.documents.remove(uri);
    }

    /// Get a document by URI
    pub fn get(&self, uri: &Url) -> Option<Document> {
        self.documents.get(uri).map(|doc| doc.clone())
    }

    /// Get all document URIs
    pub fn get_all_uris(&self) -> Vec<Url> {
        self.documents.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Check if a document exists
    pub fn contains(&self, uri: &Url) -> bool {
        self.documents.contains_key(uri)
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
