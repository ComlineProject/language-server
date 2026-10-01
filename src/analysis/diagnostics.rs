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
        ParseErrorReason::MissingToken(expected) => {
            format!("Syntax error: missing required token '{}'", expected)
        }
        ParseErrorReason::FailedNode(nested) => first_informative_message(nested)
            .unwrap_or_else(|| "Syntax error: unrecognized or incomplete syntax".to_string()),
    }
}

/// Depth-first search for the first leaf in a `FailedNode`'s nested errors
/// that actually names a token — `nested.first()` alone misses anything
/// past the first entry, a `MissingToken` first entry, or nesting more than
/// one `FailedNode` deep.
fn first_informative_message(nested: &[rust_sitter::errors::ParseError]) -> Option<String> {
    use rust_sitter::errors::ParseErrorReason;

    for err in nested {
        match &err.reason {
            ParseErrorReason::UnexpectedToken(token) => {
                return Some(format!("Unexpected token: '{}'", token));
            }
            ParseErrorReason::MissingToken(expected) => {
                return Some(format!("Syntax error: missing required token '{}'", expected));
            }
            ParseErrorReason::FailedNode(inner) => {
                if let Some(msg) = first_informative_message(inner) {
                    return Some(msg);
                }
            }
        }
    }
    None
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

    // `format_error_message` cases below, from least to most nested.
    //
    // A direct `MissingToken` (`struct User { name string }`, a bare field
    // with no `:`) is already covered by `test_generate_diagnostics_for_errors`
    // above; `missing_required_token_names_it` repeats that source to check
    // the exact message instead of just presence+severity.

    #[test]
    fn missing_required_token_names_it() {
        let source = "struct User {\n    name string\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "Syntax error: missing required token ':'");
    }

    #[test]
    fn nested_unexpected_token_names_it_without_the_old_suffix() {
        // `???` isn't a valid type, so this becomes a `FailedNode` wrapping a
        // single nested `UnexpectedToken("???")` — the shape the *old* code
        // already handled, but with a ". Check syntax around this location."
        // suffix this fix deliberately drops for consistency with every other
        // nesting depth (see the PR description).
        let source = "struct User {\n    name: ???\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "Unexpected token: '???'");
    }

    #[test]
    fn nested_unexpected_token_picks_the_first_in_document_order() {
        // Two sibling problems in one malformed block: a bad field type
        // (`int` with no leading `:`) followed by stray garbage (`$$$`).
        // `nested.first()` alone would already get this right since `int` is
        // literally first — the point of this test is pinning down that
        // first-in-document-order (not "most severe" or "last") is the
        // intended, and actual, behavior.
        let source = "struct User {\n    name int\n    $$$\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "Unexpected token: 'int'");
    }

    #[test]
    fn empty_failed_node_falls_back_to_a_generic_message() {
        // A `FailedNode` can carry zero nested errors — tree-sitter inserts a
        // zero-width error node with no text, e.g. for input that cuts off
        // mid-construct with nothing recognizable left to point at (here: an
        // unclosed `struct` body). Nothing instructive exists to surface.
        let source = "struct User {\n    name: string\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "Syntax error: unrecognized or incomplete syntax");
    }

    // The two cases below construct `ParseError` values directly rather than
    // parsing real source, unlike every test above. `MissingToken` and a
    // second level of `FailedNode` nesting are reachable in rust-sitter's own
    // type (see `rust_sitter::errors::collect_parsing_errors`: a "missing"
    // child *can* turn up inside an error node's own children, and an error
    // node's children can themselves be error nodes), but neither shape was
    // reproducible through any malformed Comline source tried here — tree-sitter
    // only ever synthesized `MissingToken` as a direct top-level substitution
    // for this grammar, never nested. The recursion has to handle both shapes
    // correctly regardless of how rarely they occur in practice, so these
    // exercise `first_informative_message` directly instead of waiting on a
    // triggering input that may not exist for this grammar.

    #[test]
    fn nested_missing_token_is_found_inside_a_failed_node() {
        use rust_sitter::errors::{ParseError, ParseErrorReason};

        let error = ParseError {
            reason: ParseErrorReason::FailedNode(vec![ParseError {
                reason: ParseErrorReason::MissingToken(":".to_string()),
                start: 5,
                end: 5,
            }]),
            start: 0,
            end: 10,
        };
        assert_eq!(
            format_error_message(&error),
            "Syntax error: missing required token ':'"
        );
    }

    #[test]
    fn doubly_nested_failed_node_recurses_to_the_leaf() {
        use rust_sitter::errors::{ParseError, ParseErrorReason};

        let error = ParseError {
            reason: ParseErrorReason::FailedNode(vec![ParseError {
                reason: ParseErrorReason::FailedNode(vec![ParseError {
                    reason: ParseErrorReason::UnexpectedToken("???".to_string()),
                    start: 7,
                    end: 10,
                }]),
                start: 5,
                end: 10,
            }]),
            start: 0,
            end: 10,
        };
        assert_eq!(format_error_message(&error), "Unexpected token: '???'");
    }
}

