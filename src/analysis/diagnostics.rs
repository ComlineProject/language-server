// Diagnostic generation — parse errors, `comline-core`'s validation pass,
// and missing imports

use crate::analysis::import_check::{self, MissingImport, UnresolvedImport, UnverifiedImport};
use crate::analysis::project::Project;
use crate::util::byte_range_to_lsp_range;
use comline_core::schema::idl::grammar::Document;
use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
use comline_core::schema::ir::compiler::Compile;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use comline_core::schema::ir::validation;
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Url};

/// The `code` of a missing-import diagnostic - what the quick fix in
/// `handlers::code_actions` answers to.
pub const MISSING_IMPORT: &str = "missing-import";

/// The `code` of an unresolved-import diagnostic (its "did you mean" has a
/// quick fix too).
pub const UNRESOLVED_IMPORT: &str = "unresolved-import";

/// The `code` of a dependency import that can't be checked.
pub const UNVERIFIED_IMPORT: &str = "unverified-import";

/// Semantic diagnostics from `comline-core`'s validation pass — undefined type
/// references, duplicate declarations, and the like: the same checks
/// `comline build` runs. Call only on a document that parsed cleanly.
pub fn validation_diagnostics(source: &str, document: &Document) -> Vec<Diagnostic> {
    validation_diagnostics_with(source, document, vec![])
}

/// [`validation_diagnostics`], with `extra` imports appended to the file's
/// own units first - the ones core can't see from this file alone (see
/// `analysis::import_check`).
fn validation_diagnostics_with(source: &str, document: &Document, extra: Vec<FrozenUnit>) -> Vec<Diagnostic> {
    let mut units = IncrementalInterpreter::from_declarations(document.0.clone());
    units.extend(extra);
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

/// Parse-error + validation diagnostics for `source` on its own. Validation
/// is skipped while the tree is malformed (parse errors present).
pub fn all_diagnostics(
    source: &str,
    errors: &[rust_sitter::errors::ParseError],
    document: Option<&Document>,
) -> Vec<Diagnostic> {
    let mut diagnostics = generate_diagnostics(source, errors);
    if errors.is_empty() {
        if let Some(doc) = document {
            // A project of one: no siblings, but `use` forms core can't
            // expand alone still aren't reported as unknown types.
            let uri = Url::parse("file:///schema.ids").expect("static URL");
            let project = Project::from_parsed([(&uri, source, std::sync::Arc::new(Document(doc.0.clone())))]);
            diagnostics.extend(project_diagnostics(&project, 0));
        }
    }
    diagnostics
}

/// Validation diagnostics for `project.docs[doc]` (a file that parsed
/// cleanly), using what the package's other files reveal: core's own checks,
/// told about the imports it can't see from one file, plus a
/// missing-import error - in place of core's bare "Unknown type" - where a
/// type declared in another file of the package is used without a `use`.
pub fn project_diagnostics(project: &Project, doc: usize) -> Vec<Diagnostic> {
    let here = &project.docs[doc];
    let check = import_check::check(project, doc);

    let mut diagnostics = validation_diagnostics_with(here.source, &here.document, check.scope);
    diagnostics.extend(check.missing.iter().map(|m| missing_import_diagnostic(here.source, m)));
    diagnostics.extend(check.unresolved.iter().map(|u| unresolved_import_diagnostic(here.source, u)));
    diagnostics.extend(check.unverified.iter().map(|u| unverified_import_diagnostic(here.source, u)));
    diagnostics
}

/// Worded like core's own error, so the editor and `comline check` say the
/// same thing.
fn unresolved_import_diagnostic(source: &str, unresolved: &UnresolvedImport) -> Diagnostic {
    let mut message = format!("Unresolved import — {}", unresolved.detail);
    if let Some(suggestion) = &unresolved.suggestion {
        message.push_str(&format!(" - did you mean '{suggestion}'?"));
    }
    import_diagnostic(source, unresolved.range, DiagnosticSeverity::ERROR, UNRESOLVED_IMPORT, message)
}

fn unverified_import_diagnostic(source: &str, unverified: &UnverifiedImport) -> Diagnostic {
    let message = format!("Not checked: {}", unverified.reason);
    import_diagnostic(source, unverified.range, DiagnosticSeverity::INFORMATION, UNVERIFIED_IMPORT, message)
}

fn import_diagnostic(
    source: &str,
    range: (usize, usize),
    severity: DiagnosticSeverity,
    code: &str,
    message: String,
) -> Diagnostic {
    Diagnostic {
        range: byte_range_to_lsp_range(source, range.0, range.1),
        severity: Some(severity),
        code: Some(NumberOrString::String(code.to_string())),
        code_description: None,
        source: Some("comline".to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}

fn missing_import_diagnostic(source: &str, missing: &MissingImport) -> Diagnostic {
    let name = &missing.name;
    let message = match (&missing.imported_as, missing.candidates.as_slice()) {
        (Some(alias), [first, ..]) => format!(
            "`{name}` is imported here as `{alias}` — write `{alias}`, or add `use {}`",
            first.use_path(name)
        ),
        (None, [only]) => format!(
            "`{name}` is declared in `{}` but not imported here — add `use {}`",
            only.file,
            only.use_path(name)
        ),
        (_, many) => format!(
            "`{name}` is declared in {} but not imported here — add a `use` for one of them",
            many.iter().map(|c| format!("`{}`", c.file)).collect::<Vec<_>>().join(", ")
        ),
    };

    Diagnostic {
        range: byte_range_to_lsp_range(source, missing.range.0, missing.range.1),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(MISSING_IMPORT.to_string())),
        code_description: None,
        source: Some("comline".to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}

/// Generate LSP diagnostics from parse errors. Each top-level
/// `ParseError` becomes its own `Diagnostic` (an editor wants one squiggly
/// per distinct problem, unlike `comline-core`'s own
/// [`comline_core::diagnostics::from_parse_errors`], which collapses a
/// whole `Vec<ParseError>` into the single most-informative one for a
/// one-shot CLI message) - so each entry is wrapped in a one-element slice
/// and handed to that same shared "most informative leaf" search, rather
/// than this crate keeping its own second copy of it.
///
/// An unclosed `{`/`[` is checked for first, once for the whole document
/// rather than per entry - it's a whole-document property, not a
/// per-`ParseError` one, and once found it *replaces* every other
/// diagnostic: a dangling bracket makes the parser's own recovery produce
/// unreliable (often whole-file-spanning) noise for everything after it,
/// the same way `rustc` treats an unclosed delimiter as the dominant error.
pub fn generate_diagnostics(source: &str, errors: &[rust_sitter::errors::ParseError]) -> Vec<Diagnostic> {
    if !errors.is_empty() {
        if let Some(diagnostic) = comline_core::diagnostics::find_unclosed_bracket(source) {
            return vec![parse_diagnostic(source, &diagnostic, None)];
        }
    }

    errors
        .iter()
        .map(|error| {
            let diagnostic = comline_core::diagnostics::from_parse_errors(std::slice::from_ref(error));
            parse_diagnostic(source, &diagnostic, Some((error.start, error.end)))
        })
        .collect()
}

/// Turn a `comline_core::diagnostics::Diagnostic` into an LSP one. `fallback`
/// is the raw `ParseError`'s own range - used only when `diagnostic` has no
/// span of its own (the one case left with nothing to point at: an empty
/// `FailedNode`, see core's diagnostics module).
fn parse_diagnostic(
    source: &str,
    diagnostic: &comline_core::diagnostics::Diagnostic,
    fallback: Option<(usize, usize)>,
) -> Diagnostic {
    let range = diagnostic
        .span
        .or(fallback)
        .map(|(start, end)| byte_range_to_lsp_range(source, start, end))
        .unwrap_or_default();

    let message = match &diagnostic.help {
        Some(help) => format!("{} — {help}", diagnostic.message),
        None => diagnostic.message.clone(),
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

    // Message-wording cases below, from least to most nested. The actual
    // leaf-search logic (nested `FailedNode`s, empty-`FailedNode` fallback,
    // first-in-document-order tie-break) is `comline_core::diagnostics`'s
    // own responsibility now and has its own test suite there - these just
    // confirm `generate_diagnostics` wires its wording and range through
    // correctly, not re-derive that recursion's correctness here too.

    #[test]
    fn missing_required_token_names_it() {
        let source = "struct User {\n    name string\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "missing required token `:`");
    }

    #[test]
    fn nested_unexpected_token_names_it() {
        // `???` isn't a valid type, so this becomes a `FailedNode` wrapping
        // a single nested `UnexpectedToken("???")`.
        let source = "struct User {\n    name: ???\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "unexpected token `???`");
    }

    #[test]
    fn nested_unexpected_token_picks_the_first_in_document_order() {
        // Two sibling problems in one malformed block: a bad field type
        // (`int` with no leading `:`) followed by stray garbage (`$$$`).
        // Pins down that first-in-document-order (not "most severe" or
        // "last") is the intended, and actual, behavior. `int` is also one
        // of `comline-core`'s known mistakes, so its suggestion rides
        // along too - a real, not synthetic, case of it firing.
        let source = "struct User {\n    name int\n    $$$\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(
            diagnostics[0].message,
            "unexpected token `int` — use sized integer types like `u32`, `s32`, `u64`, etc."
        );
    }

    #[test]
    fn an_unclosed_struct_body_names_the_brace_not_a_generic_message() {
        // Input that cuts off mid-construct with an unclosed `{` used to
        // fall all the way back to a bare "unrecognized or incomplete
        // syntax", spanning the entry's own (often whole-file) range -
        // `find_unclosed_bracket` now catches this directly and names the
        // actual problem instead.
        let source = "struct User {\n    name: string\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics[0].message, "unclosed `{` — add a matching `}` to close this block");
    }

    #[test]
    fn a_known_mistake_surfaces_its_suggestion_in_the_message() {
        // `string` parses fine as an ordinary (if semantically wrong)
        // `Type::Named` identifier - it's never actually the text of a
        // real `UnexpectedToken`, so this constructs one directly, same as
        // `comline-core`'s own equivalent test. What this confirms is that
        // `comline_core::diagnostics`'s "did you mean `str`?" suggestion
        // now actually reaches the editor, appended to the message -
        // before this convergence the LSP had no suggestion logic at all.
        use rust_sitter::errors::{ParseError, ParseErrorReason};

        let error = ParseError {
            reason: ParseErrorReason::UnexpectedToken("string".to_string()),
            start: 0,
            end: 6,
        };
        let diagnostics = generate_diagnostics("string", std::slice::from_ref(&error));
        assert_eq!(diagnostics[0].message, "unexpected token `string` — did you mean `str`?");
    }

    #[test]
    fn idp_parse_errors_flow_through_generate_diagnostics() {
        // generate_diagnostics takes a plain `&[ParseError]`, not an
        // `.ids`-specific type — confirms it's genuinely reusable for
        // `.idp`'s parse errors, not just incidentally compatible.
        let source = "congregation test\nspecification_version =\n";
        let result = crate::parser::parse_idp(source).unwrap();
        assert!(result.has_errors());

        let diagnostics = generate_diagnostics(source, &result.errors);
        assert!(!diagnostics.is_empty());
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostics[0].source, Some("comline".to_string()));
    }

    #[test]
    fn a_well_formed_idp_file_has_no_parse_errors() {
        let source = "congregation test\nspecification_version = 1\n";
        let result = crate::parser::parse_idp(source).unwrap();
        assert!(!result.has_errors());
        assert!(generate_diagnostics(source, &result.errors).is_empty());
    }

    fn messages(source: &str) -> Vec<String> {
        let r = parser::parse(source).unwrap();
        all_diagnostics(source, &r.errors, r.document.as_ref()).into_iter().map(|d| d.message).collect()
    }

    fn project_messages(files: &[(&str, &str)]) -> Vec<(String, Option<NumberOrString>, u32, u32)> {
        let files: Vec<(Url, String)> =
            files.iter().map(|(u, s)| (Url::parse(u).unwrap(), s.to_string())).collect();
        let project = Project::new(files.iter());
        project_diagnostics(&project, 0)
            .into_iter()
            .map(|d| (d.message, d.code, d.range.start.line, d.range.start.character))
            .collect()
    }

    const TYPES: (&str, &str) = ("file:///pkg/src/types.ids", "struct Message {\n    text: string\n}\n");

    #[test]
    fn use_forms_core_cannot_expand_alone_are_not_unknown_types() {
        // Each of these was reported as "Unknown type 'Message'" on its own.
        for source in [
            "use types::*\n\nstruct S {\n    m: Message\n}\n",
            "use types::{Message, Other}\n\nstruct S {\n    m: Message\n}\n",
            "use types\n\nstruct S {\n    m: Message\n}\n",
            "use types\n\nstruct S {\n    m: types::Message\n}\n",
        ] {
            assert_eq!(messages(source), Vec::<String>::new(), "{source}");
        }
    }

    /// `use parent::*` used to be a raw parser error (core#65).
    #[test]
    fn a_bare_prefix_glob_or_list_is_not_a_syntax_error() {
        for header in ["use parent::*", "use self::*", "use package::{Message}", "use parent::{Message, Other}"] {
            let source = format!("{header}\n\nstruct S {{\n    id: u64\n}}\n");
            assert_eq!(messages(&source), Vec::<String>::new(), "{header}");
        }
    }

    #[test]
    fn a_type_used_with_no_import_at_all_is_still_unknown_on_its_own() {
        assert_eq!(messages("struct S {\n    m: Message\n}\n").len(), 1);
    }

    #[test]
    fn a_missing_import_replaces_the_unknown_type_error() {
        let found = project_messages(&[("file:///pkg/src/chat.ids", "struct S {\n    m: Message\n}\n"), TYPES]);
        assert_eq!(
            found,
            vec![(
                "`Message` is declared in `types.ids` but not imported here — add `use types::Message`".to_string(),
                Some(NumberOrString::String(MISSING_IMPORT.to_string())),
                1,
                7
            )]
        );
    }

    #[test]
    fn a_glob_of_a_project_file_is_expanded_with_what_it_declares() {
        let chat = "use types::*\n\nstruct S {\n    m: Message\n    n: Nope\n}\n";
        let found = project_messages(&[("file:///pkg/src/chat.ids", chat), TYPES]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].0.starts_with("Unknown type 'Nope'"), "{found:?}");
    }

    #[test]
    fn using_the_real_name_of_an_aliased_import_says_so() {
        let chat = "use types::Message as Msg\n\nstruct S {\n    m: Message\n}\n";
        let found = project_messages(&[("file:///pkg/src/chat.ids", chat), TYPES]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "`Message` is imported here as `Msg` — write `Msg`, or add `use types::Message`");
    }
}
