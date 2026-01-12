// Diagnostic generation from parse errors

use crate::util::byte_range_to_lsp_range;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

/// Generate LSP diagnostics from parse errors
pub fn generate_diagnostics(source: &str, errors: &[rust_sitter::errors::ParseError]) -> Vec<Diagnostic> {
    errors
        .iter()
        .map(|error| {
            // Convert byte offsets to LSP range
            let range = byte_range_to_lsp_range(source, error.start, error.end);
            
            // Generate human-friendly message
            let message = format_error_message(error);
            
            Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                code: None,
                code_description: None,
                source: Some("comline".to_string()),
                message,
                related_information: None,
                tags: None,
                data: None,
            }
        })
        .collect()
}

/// Format parse error into human-friendly message
fn format_error_message(error: &rust_sitter::errors::ParseError) -> String {
    use rust_sitter::errors::ParseErrorReason;
    
    match &error.reason {
        ParseErrorReason::UnexpectedToken(token) => {
            format!("Unexpected token: '{}'", token)
        }
        ParseErrorReason::FailedNode(nested) => {
            if let Some(first_error) = nested.first() {
                if let ParseErrorReason::UnexpectedToken(token) = &first_error.reason {
                    return format!("Unexpected token: '{}'. Check syntax around this location.", token);
                }
            }
            "Syntax error: failed to parse".to_string()
        }
        ParseErrorReason::MissingToken(expected) => {
            format!("Syntax error: missing required token '{}'", expected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;
    
    #[test]
    fn test_generate_diagnostics_for_errors() {
        let source = r#"
struct User {
    name string
}
"#;
        let result = parser::parse(source).unwrap();
        assert!(result.has_errors());
        
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert!(!diagnostics.is_empty());
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
    }
    
    #[test]
    fn test_no_diagnostics_for_valid_code() {
        let source = r#"
struct User {
    name: string
}
"#;
        let result = parser::parse(source).unwrap();
        assert!(!result.has_errors());
        
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert!(diagnostics.is_empty());
    }
}
