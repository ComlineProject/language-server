// Diagnostic generation — parse errors, and `comline-core`'s validation pass

use crate::util::byte_range_to_lsp_range;
use comline_core::schema::idl::grammar::Document;
use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
use comline_core::schema::ir::compiler::Compile;
use comline_core::schema::ir::validation;
use lsp_types::{Diagnostic, DiagnosticSeverity};

/// Semantic diagnostics from `comline-core`'s validation pass — undefined type
/// references, duplicate declarations, and the like: the same checks
/// `comline build` runs. Call only on a document that parsed cleanly.
pub fn validation_diagnostics(source: &str, document: &Document) -> Vec<Diagnostic> {
    let units = IncrementalInterpreter::from_declarations(document.0.clone());
    let errors = match validation::validate(&units) {
        Ok(()) => return vec![],
        Err(errors) => errors,
    };

    errors
        .into_iter()
        .map(|error| {
            let range = error
                .span
                .map(|(start, end)| byte_range_to_lsp_range(source, start, end))
                .unwrap_or_default();

            let message = if error.context.is_empty() {
                error.message
            } else {
                format!("{} — {}", error.message, error.context)
            };

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

/// Parse-error + validation diagnostics for `source`. Validation is skipped
/// while the tree is malformed (parse errors present).
pub fn all_diagnostics(
    source: &str,
    errors: &[rust_sitter::errors::ParseError],
    document: Option<&Document>,
) -> Vec<Diagnostic> {
    let mut diagnostics = generate_diagnostics(source, errors);
    if errors.is_empty() {
        if let Some(doc) = document {
            diagnostics.extend(validation_diagnostics(source, doc));
        }
    }
    diagnostics
}

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

    fn diags(source: &str) -> Vec<Diagnostic> {
        let result = parser::parse(source).unwrap();
        all_diagnostics(source, &result.errors, result.document.as_ref())
    }

    #[test]
    fn validation_flags_an_undefined_type_reference() {
        let d = diags("struct Order {\n    buyer: Customer\n}\n");
        assert!(
            d.iter().any(|x| x.message.to_lowercase().contains("customer")),
            "expected an undefined-type diagnostic mentioning `Customer`, got {d:?}"
        );
    }

    #[test]
    fn validation_flags_a_duplicate_declaration() {
        let d = diags("struct User {\n    a: string\n}\nstruct User {\n    b: string\n}\n");
        assert!(
            d.iter().any(|x| x.message.to_lowercase().contains("duplicate")),
            "expected a duplicate-definition diagnostic, got {d:?}"
        );
    }

    #[test]
    fn a_well_formed_schema_has_no_diagnostics() {
        let d = diags(
            "struct Item {\n    id: u64\n}\n\nstruct Cart {\n    items: Item[]\n}\n",
        );
        assert!(d.is_empty(), "expected no diagnostics, got {d:?}");
    }

    #[test]
    fn validation_is_skipped_while_the_tree_is_malformed() {
        // A parse error is present, so validation must not run (no panic, no
        // spurious semantic errors) — only the parse diagnostic.
        let source = "struct User {\n    name string\n}\n";
        let result = parser::parse(source).unwrap();
        assert!(result.has_errors());
        let d = all_diagnostics(source, &result.errors, result.document.as_ref());
        assert!(!d.is_empty());
    }
}
