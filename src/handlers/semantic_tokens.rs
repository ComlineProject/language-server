// Semantic tokens handler - provides enhanced syntax highlighting

use crate::analysis::symbols;
use crate::parser;
use comline_core::schema::idl::grammar::{Declaration, Type};
use tower_lsp::lsp_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensResult, Url,
};

/// Generate semantic tokens for a document
pub fn get_semantic_tokens(source: &str, uri: &Url) -> Option<SemanticTokensResult> {
    // Parse the document
    let parse_result = parser::parse(source).ok()?;
    let document = parse_result.document?;
    
    let mut tokens = Vec::new();
    let mut current_line = 0;
    let mut current_char = 0;
    
    // Walk through declarations and emit tokens
    for decl in &document.0 {
        match decl {
            Declaration::Struct(s) => {
                // Emit token for struct keyword and name
                if let Some(token) = create_token_for_name(
                    source,
                    &s.name(),
                    SemanticTokenType::STRUCT,
                    &mut current_line,
                    &mut current_char,
                ) {
                    tokens.push(token);
                }
                
                // Emit tokens for fields
                for field in s.fields() {
                    if let Some(token) = create_token_for_name(
                        source,
                        &field.name(),
                        SemanticTokenType::PROPERTY,
                        &mut current_line,
                        &mut current_char,
                    ) {
                        tokens.push(token);
                    }
                }
            }
            Declaration::Enum(e) => {
                if let Some(token) = create_token_for_name(
                    source,
                    &e.name(),
                    SemanticTokenType::ENUM,
                    &mut current_line,
                    &mut current_char,
                ) {
                    tokens.push(token);
                }
            }
            Declaration::Protocol(p) => {
                if let Some(token) = create_token_for_name(
                    source,
                    &p.name(),
                    SemanticTokenType::INTERFACE,
                    &mut current_line,
                    &mut current_char,
                ) {
                    tokens.push(token);
                }
                
                // Emit tokens for functions
                for func in p.functions() {
                    if let Some(token) = create_token_for_name(
                        source,
                        &func.name(),
                        SemanticTokenType::FUNCTION,
                        &mut current_line,
                        &mut current_char,
                    ) {
                        tokens.push(token);
                    }
                }
            }
            Declaration::Const(c) => {
                if let Some(token) = create_token_for_name(
                    source,
                    &c.name(),
                    SemanticTokenType::VARIABLE,
                    &mut current_line,
                    &mut current_char,
                ) {
                    tokens.push(token);
                }
            }
            _ => {}
        }
    }
    
    Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: tokens,
    }))
}

/// Create a semantic token for a name
fn create_token_for_name(
    source: &str,
    name: &str,
    token_type: SemanticTokenType,
    current_line: &mut u32,
    current_char: &mut u32,
) -> Option<SemanticToken> {
    // Find the position of this name in source
    let pos = source.find(name)?;
    
    // Convert byte offset to line/col
    let (line, col) = byte_offset_to_line_col(source, pos);
    
    // Calculate delta from last position
    let delta_line = line.saturating_sub(*current_line);
    let delta_start = if delta_line == 0 {
        col.saturating_sub(*current_char)
    } else {
        col
    };
    
    // Update current position
    *current_line = line;
    *current_char = col;
    
    // Get token type index
    let token_type_idx = match token_type {
        SemanticTokenType::KEYWORD => 0,
        SemanticTokenType::TYPE => 1,
        SemanticTokenType::STRUCT => 2,
        SemanticTokenType::ENUM => 3,
        SemanticTokenType::INTERFACE => 4,
        SemanticTokenType::FUNCTION => 5,
        SemanticTokenType::VARIABLE => 6,
        SemanticTokenType::PROPERTY => 7,
        _ => return None,
    };
    
    Some(SemanticToken {
        delta_line,
        delta_start,
        length: name.len() as u32,
        token_type: token_type_idx,
        token_modifiers_bitset: 0,
    })
}

/// Convert byte offset to line and column
fn byte_offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    let mut line = 0;
    let mut col = 0;
    
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_semantic_tokens_struct() {
        let source = r#"
struct User {
    name: string
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        
        let result = get_semantic_tokens(source, &uri);
        assert!(result.is_some());
        
        if let Some(SemanticTokensResult::Tokens(tokens)) = result {
            // Should have at least 2 tokens (struct name + field)
            assert!(tokens.data.len() >= 2);
        }
    }
    
    #[test]
    fn test_byte_offset_to_line_col() {
        let source = "line 1\nline 2\nline 3";
        
        assert_eq!(byte_offset_to_line_col(source, 0), (0, 0));
        assert_eq!(byte_offset_to_line_col(source, 7), (1, 0));
        assert_eq!(byte_offset_to_line_col(source, 14), (2, 0));
    }
}
