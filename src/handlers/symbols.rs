// Symbol provider - to be implemented

use tower_lsp::lsp_types::{DocumentSymbol, SymbolInformation};

pub fn get_document_symbols(_source: &str) -> Vec<DocumentSymbol> {
    // TODO: Implement document symbols
    vec![]
}

pub fn get_workspace_symbols(_query: &str) -> Vec<SymbolInformation> {
    // TODO: Implement workspace symbols
    vec![]
}
