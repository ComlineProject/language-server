// Rename handler - to be implemented

use tower_lsp::lsp_types::WorkspaceEdit;

pub fn rename_symbol(_source: &str, _position: usize, _new_name: &str) -> Option<WorkspaceEdit> {
    // TODO: Implement rename
    None
}
