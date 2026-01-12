// Completion handler - to be implemented

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

pub fn get_completions(_source: &str, _position: usize) -> Vec<CompletionItem> {
    // TODO: Implement completion logic
    // For now, return basic keywords
    vec![
        CompletionItem {
            label: "struct".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        },
        CompletionItem {
            label: "enum".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        },
        CompletionItem {
            label: "protocol".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        },
    ]
}
