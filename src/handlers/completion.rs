// Completion handler - provides auto-completion suggestions

use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use lsp_types::{CompletionItem, CompletionItemKind, Position, Url};

/// Get completion suggestions at a position
pub fn get_completions(source: &str, uri: &Url, position: Position) -> Vec<CompletionItem> {
    let mut completions = Vec::new();
    
    // Get context around cursor
    let offset = match position_to_offset(source, position) {
        Some(o) => o,
        None => return get_keyword_completions(),
    };
    
    // Parse the document to get available types
    if let Ok(parse_result) = parser::parse(source) {
        if let Some(document) = parse_result.document {
            // Build symbol table to get available types
            let symbol_table = symbols::build_symbol_table(&document, uri, source);
            
            // Determine context
            let context = determine_context(source, offset);
            
            match context {
                CompletionContext::TypePosition => {
                    // After ":", suggest types
                    completions.extend(get_type_completions(&symbol_table));
                }
                CompletionContext::TopLevel => {
                    // At top level, suggest declaration keywords
                    completions.extend(get_keyword_completions());
                }
                CompletionContext::StructBody => {
                    // Inside struct, suggest "optional" keyword
                    completions.push(CompletionItem {
                        label: "optional".to_string(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        detail: Some("Optional field modifier".to_string()),
                        ..Default::default()
                    });
                }
                CompletionContext::Unknown => {
                    // Default: provide basic suggestions
                    completions.extend(get_keyword_completions());
                    completions.extend(get_type_completions(&symbol_table));
                }
            }
        }
    } else {
        // If parsing fails, just offer keywords
        completions.extend(get_keyword_completions());
    }
    
    // Always include primitive types
    completions.extend(get_primitive_type_completions());
    
    completions
}

/// Determine completion context based on position
fn determine_context(source: &str, offset: usize) -> CompletionContext {
    if offset == 0 || offset > source.len() {
        return CompletionContext::TopLevel;
    }
    
    // Look backwards to determine context
    let before = &source[..offset];
    
    // Check if we're after a colon (type position)
    if before.trim_end().ends_with(':') {
        return CompletionContext::TypePosition;
    }
    
    // Check if we're in a struct body
    if is_in_struct_body(before) {
        return CompletionContext::StructBody;
    }
    
    // Check if we're at top level (no unclosed braces)
    let open_braces = before.matches('{').count();
    let close_braces = before.matches('}').count();
    
    if open_braces == close_braces {
        CompletionContext::TopLevel
    } else {
        CompletionContext::Unknown
    }
}

/// Check if position is inside a struct body
fn is_in_struct_body(text: &str) -> bool {
    // Simple heuristic: last "struct" keyword is before last "{"
    if let Some(last_struct) = text.rfind("struct ") {
        if let Some(last_brace) = text.rfind('{') {
            if last_brace > last_struct {
                // Check if we haven't closed this struct yet
                let after_brace = &text[last_brace..];
                return !after_brace.contains('}');
            }
        }
    }
    false
}

/// Get keyword completions (struct, enum, protocol, etc.)
fn get_keyword_completions() -> Vec<CompletionItem> {
    vec![
        CompletionItem {
            label: "struct".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a structure".to_string()),
            insert_text: Some("struct $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "enum".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define an enumeration".to_string()),
            insert_text: Some("enum $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "protocol".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a protocol".to_string()),
            insert_text: Some("protocol $1 {\n\t$0\n}".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "const".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Define a constant".to_string()),
            insert_text: Some("const $1: $2 = $0".to_string()),
            insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
            ..Default::default()
        },
        CompletionItem {
            label: "use".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Import statement".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "import".to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("Legacy import statement".to_string()),
            ..Default::default()
        },
    ]
}

/// Get primitive type completions
fn get_primitive_type_completions() -> Vec<CompletionItem> {
    vec![
        // Integer types
        CompletionItem {
            label: "i8".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("8-bit signed integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "i16".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("16-bit signed integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "i32".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("32-bit signed integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "i64".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("64-bit signed integer".to_string()),
            ..Default::default()
        },
        // Unsigned integers
        CompletionItem {
            label: "u8".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("8-bit unsigned integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "u16".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("16-bit unsigned integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "u32".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("32-bit unsigned integer".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "u64".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("64-bit unsigned integer".to_string()),
            ..Default::default()
        },
        // Floats
        CompletionItem {
            label: "f32".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("32-bit floating point".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "f64".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("64-bit floating point".to_string()),
            ..Default::default()
        },
        // Strings and bool
        CompletionItem {
            label: "string".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("String type".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "str".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("String slice type".to_string()),
            ..Default::default()
        },
        CompletionItem {
            label: "bool".to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some("Boolean type".to_string()),
            ..Default::default()
        },
    ]
}

/// Get user-defined type completions from symbol table
fn get_type_completions(symbol_table: &symbols::SymbolTable) -> Vec<CompletionItem> {
    symbol_table
        .all_symbols()
        .iter()
        .map(|symbol| {
            let kind = match symbol.kind {
                lsp_types::SymbolKind::STRUCT => CompletionItemKind::STRUCT,
                lsp_types::SymbolKind::ENUM => CompletionItemKind::ENUM,
                lsp_types::SymbolKind::INTERFACE => CompletionItemKind::INTERFACE,
                _ => CompletionItemKind::CLASS,
            };
            
            let detail = if !symbol.children.is_empty() {
                Some(format!("{} - {} items", symbol.name, symbol.children.len()))
            } else {
                None
            };
            
            CompletionItem {
                label: symbol.name.clone(),
                kind: Some(kind),
                detail,
                ..Default::default()
            }
        })
        .collect()
}

/// Completion context
#[derive(Debug, PartialEq)]
enum CompletionContext {
    TypePosition,   // After ":"
    TopLevel,       // At file top level
    StructBody,     // Inside struct definition
    Unknown,        // Unknown context
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_keyword_completions() {
        let completions = get_keyword_completions();
        assert!(completions.len() >= 5);
        assert!(completions.iter().any(|c| c.label == "struct"));
        assert!(completions.iter().any(|c| c.label == "enum"));
        assert!(completions.iter().any(|c| c.label == "protocol"));
    }
    
    #[test]
    fn test_primitive_type_completions() {
        let completions = get_primitive_type_completions();
        assert!(completions.len() >= 10);
        assert!(completions.iter().any(|c| c.label == "i32"));
        assert!(completions.iter().any(|c| c.label == "string"));
        assert!(completions.iter().any(|c| c.label == "bool"));
    }
    
    #[test]
    fn test_completion_after_colon() {
        let source = "struct User {\n    name: ";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 10); // After ":"
        
        let completions = get_completions(source, &uri, position);
        // Should include types
        assert!(!completions.is_empty());
        assert!(completions.iter().any(|c| c.label == "string"));
    }
    
    #[test]
    fn test_completion_top_level() {
        let source = "\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(0, 0);
        
        let completions = get_completions(source, &uri, position);
        // Should include keywords
        assert!(completions.iter().any(|c| c.label == "struct"));
        assert!(completions.iter().any(|c| c.label == "enum"));
    }
    
    #[test]
    fn test_context_detection() {
        assert_eq!(determine_context("name: ", 6), CompletionContext::TypePosition);
        assert_eq!(determine_context("", 0), CompletionContext::TopLevel);
        assert_eq!(determine_context("struct User {\n    ", 18), CompletionContext::StructBody);
    }
}
