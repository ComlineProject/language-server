// Diagnostic generation — parse errors, `comline-core`'s validation pass,
// and missing imports

use std::collections::BTreeSet;

use crate::analysis::generation;
use crate::analysis::import_check::{self, MissingImport, UnresolvedImport, UnverifiedImport};
use crate::analysis::project::Project;
use crate::analysis::symbols::Namespace;
use crate::util::byte_range_to_lsp_range;
use comline_core::diagnostics::Severity;
use comline_core::schema::idl::grammar::Document;
use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
use comline_core::schema::ir::compiler::settings::enforcement::check_settings_enforcement;
use comline_core::schema::ir::compiler::settings::resolution::check_settings_conflicts;
use comline_core::schema::ir::compiler::settings::severity::resolve_diagnostic_overrides;
use comline_core::schema::ir::compiler::import_resolver::{
    declared_alias_target, declared_validator_properties, declared_validator_targets,
};
use comline_core::schema::ir::compiler::Compile;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use comline_core::schema::ir::frozen::types::TypeRef;
use comline_core::package::layout;
use comline_core::schema::ir::validation;
use comline_core::settings::{SettingsDict, SettingsSpans};
use lsp_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, DiagnosticTag, Location,
    NumberOrString, Range, Url,
};

/// The `code` of a missing-import diagnostic - what the quick fix in
/// `handlers::code_actions` answers to.
pub const MISSING_IMPORT: &str = "import::missing";

/// The `code` of an unresolved-import diagnostic (its "did you mean" has a
/// quick fix too).
pub const UNRESOLVED_IMPORT: &str = "import::unresolved";

/// The `code` of a dependency import that can't be checked.
pub const UNVERIFIED_IMPORT: &str = "import::unverified";

/// The `code` of a schema file whose own name can't be used - an IDL
/// keyword, or a name a target language's module system has spoken for.
pub const RESERVED_SCHEMA_NAME: &str = "schema::reserved-name";

/// The `code` of a name declared twice in one schema - a struct and an
/// error, two errors, two validators. Both the quick fixes in
/// `handlers::code_actions` and the span narrowing below answer to it.
pub const DUPLICATE_DECLARATION: &str = "validation::duplicate-declaration";

/// The `code` of a `settings` block declared twice in one schema. Treated
/// exactly like [`DUPLICATE_DECLARATION`] here - same shape of problem, same
/// two-site report.
pub const DUPLICATE_BLOCK: &str = "settings::duplicate-block";

/// `comline_core::diagnostics::Severity` -> the matching LSP tier - direct
/// 1:1, no surprises (the two enums were designed to line up exactly).
/// Diagnostics core spans over a whole declaration, which the editor shows
/// at its name instead: a duplicate, and what's said about a validator as a
/// whole (its targets, its asserts' references, an empty `validate`).
fn narrows_to_name(code: &str) -> bool {
    matches!(
        code,
        DUPLICATE_DECLARATION
            | DUPLICATE_BLOCK
            | "validation::validator-checks-nothing"
            | "validation::validator-missing-target"
            | "validation::validator-accessor-unsupported"
            | "validation::unknown-validator-property"
            | "validation::invalid-validator-reference"
    )
}

fn lsp_severity(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

/// Where the `HINT` half of a two-site diagnostic sends the reader. The
/// wording used to name the settings entry unconditionally - true of the
/// only two-site diagnostic there was when it was written, and wrong the
/// moment a duplicate declaration became another.
fn hint_pointer(code_id: &str) -> &'static str {
    match code_id {
        DUPLICATE_DECLARATION | DUPLICATE_BLOCK => "see the error at the duplicate",
        _ => "see the error at the settings entry",
    }
}

/// The LSP tags a diagnostic's own code implies, if any - `DEPRECATED`
/// makes VS Code strike through the affected range, the standard editor
/// presentation for "this still works, but don't use it" (same as
/// rust-analyzer on a deprecated-item reference). Every other code keeps
/// `tags: None`.
fn lsp_tags(code_id: &str) -> Option<Vec<DiagnosticTag>> {
    match code_id {
        "validation::deprecated-type-usage" => Some(vec![DiagnosticTag::DEPRECATED]),
        _ => None,
    }
}

/// `document` frozen the way validation reads it: its own units, then
/// `extra` (imports core can't see from one file alone), behind a
/// `Namespace` unit when the module path is known.
fn frozen_units(document: &Document, extra: Vec<FrozenUnit>, namespace: &[String]) -> Vec<FrozenUnit> {
    let mut units = IncrementalInterpreter::from_declarations(document.0.clone());
    units.extend(extra);
    if !namespace.is_empty() {
        // `from_declarations` has no project context and so emits no
        // `Namespace` unit of its own; the project does know the file's
        // module path, so core gets told the same thing a real build knows.
        units.insert(0, FrozenUnit::Namespace(namespace.join("::")));
    }
    units
}

/// Semantic diagnostics from `comline-core`'s validation pass — undefined type
/// references, duplicate declarations, and the like: the same checks
/// `comline build` runs. Call only on a document that parsed cleanly.
pub fn validation_diagnostics(source: &str, document: &Document) -> Vec<Diagnostic> {
    let uri = Url::parse("file:///schema.ids").expect("static URL");
    validation_diagnostics_with(
        source,
        document,
        &frozen_units(document, vec![], &[]),
        &crate::analysis::source::base_settings(),
        &uri,
        &validation::NoImports,
    )
}

/// What core's validation asks about names a file imports - an imported
/// validator's `for` targets and properties, what an imported `type` alias
/// stands for -
/// answered from the declaration the file's own scope brings in
/// ([`Project::resolve`]), read with core's own helpers so the
/// editor resolves exactly as `comline build` does.
struct ProjectImports<'p, 'a> {
    project: &'p Project<'a>,
    doc: usize,
    /// Names [`import_check`] gave a stand-in import because their `use` is
    /// missing - reported as such, never judged against a validator.
    stand_ins: BTreeSet<String>,
}

impl ProjectImports<'_, '_> {
    fn declaration(&self, name: &str) -> Option<(usize, String)> {
        let bare = name.rsplit("::").next()?;
        // In scope only, as `comline build` resolves: the project-wide
        // fallback would answer for a validator this file never imported.
        let target = self.project.resolve(self.doc, bare, Namespace::Declaration)?;
        let real_name = self.project.symbol(&target)?.name.clone();
        Some((target.doc, real_name))
    }
}

impl validation::ImportLookup for ProjectImports<'_, '_> {
    fn validator_targets(&self, name: &str) -> Option<Vec<TypeRef>> {
        let (doc, real_name) = self.declaration(name)?;
        declared_validator_targets(&self.project.docs[doc].document.0, &real_name)
    }

    fn alias_target(&self, name: &str) -> Option<TypeRef> {
        let (doc, real_name) = self.declaration(name)?;
        declared_alias_target(&self.project.docs[doc].document.0, &real_name)
    }

    fn validator_properties(&self, name: &str) -> Option<Vec<FrozenUnit>> {
        let (doc, real_name) = self.declaration(name)?;
        declared_validator_properties(&self.project.docs[doc].document.0, &real_name)
    }

    fn is_stand_in(&self, name: &str) -> bool {
        self.stand_ins.contains(name)
    }
}

/// [`validation_diagnostics`], with `extra` imports appended to the file's
/// own units first - the ones core can't see from this file alone (see
/// `analysis::import_check`) - `package_settings`, the package-level
/// settings dict (empty when there's no manifest in view) checked alongside
/// core's own validation - and `uri`, this document's own URI, needed to
/// build a same-file `related_information` location for a two-site error
/// (see the `secondary`-span handling below), and `namespace`, this
/// schema's own module path - core names a declaration by it (`error
/// 'test::main::Test'`), and without it the editor would say `'Test'` where
/// `comline build` says the qualified form.
fn validation_diagnostics_with(
    source: &str,
    document: &Document,
    units: &[FrozenUnit],
    package_settings: &SettingsDict,
    uri: &Url,
    imports: &dyn validation::ImportLookup,
) -> Vec<Diagnostic> {
    // The `settings` block's own checks - duplicate and conflicting keys,
    // a bad `mode` value, an unrecognised key, a wrongly-typed value. The
    // compile path runs these; this path called plain `from_declarations`,
    // which skips them, so none of them had ever appeared in the editor.
    // They are reported here rather than by switching to
    // `check_and_from_declarations`, whose import and alias checks would
    // duplicate `analysis::import_check`'s own.
    let settings_errors =
        check_settings_conflicts(&document.0).err().unwrap_or_default();

    let mut errors = match validation::validate_with(units, imports) {
        Ok(warnings) => {
            // No real build to fail here, unlike the CLI's compile pass -
            // an "error"-escalated or malformed-level diagnostic just
            // renders as a real LSP error alongside everything else.
            let (warnings, fatal) = resolve_diagnostic_overrides(
                units, warnings, package_settings, &SettingsSpans::default(),
            );
            let mut combined = warnings;
            combined.extend(fatal);
            combined
        }
        Err(errors) => errors,
    };
    errors.extend(
        check_settings_enforcement(units, package_settings, &SettingsSpans::default())
            .err()
            .unwrap_or_default(),
    );

    // Where both passes have an opinion on the same written value, keep
    // enforcement's. The two legitimately overlap: the block-level check
    // above sees every value eagerly, while enforcement only sees the keys
    // it actually consults - but reports those against the declaration
    // affected as well as the entry itself, which is the more useful of
    // the two. Same code at the same origin means the same mistake, so
    // only the ones enforcement said nothing about are added.
    let already: Vec<_> =
        errors.iter().map(|e| (e.code.id, e.span)).collect();
    errors.extend(
        settings_errors
            .into_iter()
            .filter(|e| !already.contains(&(e.code.id, e.span))),
    );

    if errors.is_empty() {
        return vec![];
    }

    // Core spans a declaration whole, having no span for the name itself
    // (`grammar::Identifier` carries none). For a duplicate that means two
    // entire blocks underlined; the editor has the exact name ranges, so it
    // narrows both ends back down to them. Built only when something
    // actually needs it - this runs on every keystroke.
    let table = errors
        .iter()
        .any(|e| narrows_to_name(e.code.id))
        .then(|| crate::analysis::symbols::build_symbol_table(document, uri, source));
    let names = |span: (usize, usize)| -> (usize, usize) {
        table.as_ref().and_then(|t| t.name_span_of_decl(span)).unwrap_or(span)
    };

    errors
        .into_iter()
        .flat_map(|error| {
            let narrow = narrows_to_name(error.code.id);
            let error = if narrow {
                let span = error.span.map(&names);
                let secondary =
                    error.secondary.map(|(span, label)| (names(span), label));
                comline_core::diagnostics::Diagnostic { span, secondary, ..error }
            } else {
                error
            };
            let range = error
                .span
                .map(|(start, end)| byte_range_to_lsp_range(source, start, end))
                .unwrap_or_default();
            let code = error.code;
            let effective_severity = error.effective_severity();
            let lsp_code = Some(NumberOrString::String(code.id.to_string()));

            // `help` carries the actionable half - a "did you mean" for a
            // misspelled settings key, say. The other conversion in this
            // module already appended it; this one dropped it, so the
            // suggestion never reached the editor.
            let mut message = if error.context.is_empty() {
                error.message
            } else {
                format!("{} — {}", error.message, error.context)
            };
            if let Some(help) = &error.help {
                message = format!("{message} — {help}");
            }

            // No secondary span: today's single-diagnostic shape, unchanged.
            let Some((sec_span, sec_label)) = &error.secondary else {
                return vec![Diagnostic {
                    range,
                    severity: Some(lsp_severity(effective_severity)),
                    code: lsp_code,
                    code_description: None,
                    source: Some("comline".to_string()),
                    message,
                    related_information: None,
                    tags: lsp_tags(code.id),
                    data: None,
                }];
            };

            // A two-site error (e.g. a malformed settings value): the real
            // error, at its origin, plus a related-information link to
            // where it's used - and a second, `HINT`-severity diagnostic
            // right at that usage site, rendered by the editor as a faint
            // dotted underline (standard LSP client behavior, no extension
            // changes needed) pointing back to the real error. Both share
            // the same code - they describe one problem from two vantage
            // points, so "jump to all diagnostics with this code" should
            // find both.
            let sec_range = byte_range_to_lsp_range(source, sec_span.0, sec_span.1);
            let primary = Diagnostic {
                range,
                severity: Some(lsp_severity(effective_severity)),
                code: lsp_code.clone(),
                code_description: None,
                source: Some("comline".to_string()),
                message: message.clone(),
                related_information: Some(vec![DiagnosticRelatedInformation {
                    location: Location { uri: uri.clone(), range: sec_range },
                    message: sec_label.clone(),
                }]),
                tags: lsp_tags(code.id),
                data: None,
            };
            let hint = Diagnostic {
                range: sec_range,
                severity: Some(DiagnosticSeverity::HINT),
                code: lsp_code,
                code_description: None,
                source: Some("comline".to_string()),
                message: format!("{sec_label} — {}", hint_pointer(code.id)),
                related_information: Some(vec![DiagnosticRelatedInformation {
                    location: Location { uri: uri.clone(), range },
                    message,
                }]),
                tags: None,
                data: None,
            };
            vec![primary, hint]
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
    let stand_ins = check.missing.iter().map(|m| m.name.clone()).collect();

    let units = frozen_units(&here.document, check.scope, &here.namespace);
    let mut diagnostics = validation_diagnostics_with(
        here.source,
        &here.document,
        &units,
        &project.settings,
        here.uri,
        &ProjectImports { project, doc, stand_ins },
    );
    // A dependency's schemas aren't generated by this package.
    if here.dependency.is_none() {
        diagnostics.extend(generation::feature_diagnostics(here.source, &units, &project.languages));
    }
    diagnostics.extend(reserved_name_diagnostic(here));
    diagnostics.extend(check.missing.iter().map(|m| missing_import_diagnostic(here.source, m)));
    diagnostics.extend(check.unresolved.iter().map(|u| unresolved_import_diagnostic(here.source, u)));
    diagnostics.extend(check.unverified.iter().map(|u| unverified_import_diagnostic(here.source, u)));
    diagnostics
}

/// A file whose own name core refuses to compile.
///
/// Core enforces this on the build path, which the language server does not
/// run, so without this the editor would stay silent about a file that
/// cannot build at all - the one disagreement with `comline build` worth
/// going out of the way to avoid. The whole file is what's wrong, so it's
/// reported at the top rather than at some span inside it.
fn reserved_name_diagnostic(here: &crate::analysis::project::ProjectDoc) -> Option<Diagnostic> {
    // A dependency's own schemas are not this package's to rename, and
    // nothing here is editing them.
    if here.dependency.is_some() {
        return None;
    }
    let (segment, reason) = layout::reserved_in_namespace(&here.namespace)?;
    Some(Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::ERROR),
        code: Some(NumberOrString::String(RESERVED_SCHEMA_NAME.to_string())),
        code_description: None,
        source: Some("comline".to_string()),
        message: format!(
            "this schema uses the reserved name '{segment}' - {}",
            reason.help(),
        ),
        related_information: None,
        tags: None,
        data: None,
    })
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
///
/// A `->` with no type after it is named next. The parser's own entry for
/// it is a bare error node over the `->` (or the whole file, when the `->`
/// ends a block), so each such entry is dropped in favour of the named
/// diagnostic rather than shown alongside it.
pub fn generate_diagnostics(source: &str, errors: &[rust_sitter::errors::ParseError]) -> Vec<Diagnostic> {
    if errors.is_empty() {
        return Vec::new();
    }
    if let Some(diagnostic) = comline_core::diagnostics::find_unclosed_bracket(source) {
        return vec![parse_diagnostic(source, &diagnostic, None)];
    }

    let missing_returns = comline_core::diagnostics::find_missing_return_types(source);
    let covers_a_missing_return = |error: &rust_sitter::errors::ParseError| {
        missing_returns
            .iter()
            .filter_map(|d| d.span)
            .any(|(start, end)| error.start <= start && end <= error.end)
    };

    errors
        .iter()
        .filter(|error| !covers_a_missing_return(error))
        .map(|error| {
            let diagnostic = comline_core::diagnostics::from_parse_errors(std::slice::from_ref(error));
            parse_diagnostic(source, &diagnostic, Some((error.start, error.end)))
        })
        .chain(missing_returns.iter().map(|d| parse_diagnostic(source, d, None)))
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

    /// The text a diagnostic's range covers.
    fn covered<'s>(source: &'s str, d: &Diagnostic) -> &'s str {
        let offset = |p: lsp_types::Position| {
            source.lines().take(p.line as usize).map(|l| l.len() + 1).sum::<usize>() + p.character as usize
        };
        &source[offset(d.range.start)..offset(d.range.end)]
    }

    #[test]
    fn a_validator_that_checks_nothing_warns_at_its_name() {
        let source = "validator Positive for u8 {\n    floor: u8 = 0\n}\n";
        let d = diags(source);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::WARNING));
        assert!(d[0].message.starts_with("validator 'Positive' checks nothing"), "{d:?}");
        assert_eq!(covered(source, &d[0]), "Positive");
    }

    #[test]
    fn an_unused_validator_property_warns_at_the_property() {
        let source = "validator Bounds for u32 {\n    min: u32 = 0\n    max: u32 = 9\n    validate {\n        assert(value >= params.min, \"low\")\n    }\n}\n";
        let d = diags(source);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(covered(source, &d[0]), "max: u32 = 9");
    }

    #[test]
    fn a_bad_reference_in_validate_is_shown_at_the_validators_name() {
        // It had no span, so the editor put it at the top of the file.
        let source = "struct S {\n    a: u8\n}\n\nvalidator Short for string {\n    validate {\n        assert(value.length <= params.mx, \"long\")\n    }\n}\n";
        let d = diags(source);
        let e = d.iter().find(|x| x.message.contains("params.mx")).expect("the error");
        assert_eq!(covered(source, e), "Short");
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
    fn an_arrow_with_no_type_names_the_missing_return_type() {
        // `-> ! E;` gives the parser nothing to name - just an empty error
        // node over the `->`. The editor gets one diagnostic, on the `->`,
        // saying what is missing instead of "unrecognized or incomplete".
        let source = "protocol P {\n    function greet(a: u8) -> ! E;\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0].message.starts_with("missing return type after `->`"),
            "{}",
            diagnostics[0].message
        );
        assert_eq!(diagnostics[0].range.start, lsp_types::Position::new(1, 26));
        assert_eq!(diagnostics[0].range.end, lsp_types::Position::new(1, 28));
    }

    #[test]
    fn an_arrow_ending_a_block_replaces_the_whole_file_error() {
        // With the `->` right before the `}`, the parser's error spans the
        // whole file - that one is the same mistake, so it goes too.
        let source = "protocol P {\n    function greet(a: u8) ->\n}\n";
        let result = parser::parse(source).unwrap();
        let diagnostics = generate_diagnostics(source, &result.errors);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].message.starts_with("missing return type after `->`"));
    }

    #[test]
    fn a_known_mistake_surfaces_its_suggestion_in_the_message() {
        // `int` parses fine as an ordinary (if unknown) `Type::Named`
        // identifier - it's never actually the text of a real
        // `UnexpectedToken`, so this constructs one directly, same as
        // `comline-core`'s own equivalent test. What this confirms is that
        // `comline_core::diagnostics`'s suggestion for it reaches the
        // editor, appended to the message - before this convergence the LSP
        // had no suggestion logic at all.
        use rust_sitter::errors::{ParseError, ParseErrorReason};

        let error = ParseError {
            reason: ParseErrorReason::UnexpectedToken("int".to_string()),
            start: 0,
            end: 3,
        };
        let diagnostics = generate_diagnostics("int", std::slice::from_ref(&error));
        assert_eq!(
            diagnostics[0].message,
            "unexpected token `int` — use sized integer types like `u32`, `s32`, `u64`, etc."
        );
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

    #[test]
    fn an_unrecognised_settings_key_is_reported_in_the_editor() {
        // The editor ran none of the `settings` block's own checks, so a
        // typo here produced no diagnostic at all - the key just silently
        // did nothing.
        let found = messages(
            "struct Message {\n    body: string\n}\n\n\
             settings {\n    annotations.framing.alowed = true\n}\n",
        );
        assert!(
            found.iter().any(|m| m.contains("annotations.framing.allowed")),
            "expected a suggestion: {found:?}"
        );
    }

    #[test]
    fn a_conflicting_settings_key_is_reported_in_the_editor() {
        // `settings::conflict` had never surfaced in the editor either.
        let found = messages(
            "struct Message {\n    body: string\n}\n\n\
             settings {\n    validators.allowed = true\n    validators.allowed = false\n}\n",
        );
        assert!(!found.is_empty(), "a conflicting key should be reported");
    }

    #[test]
    fn an_open_namespace_settings_key_is_still_not_reported() {
        // The flip side: `settings` stays open outside the groups core
        // owns, so an unrecognised *top-level* key must stay quiet.
        let found = messages(
            "struct Message {\n    body: string\n}\n\n\
             settings {\n    forbid_indexing = true\n}\n",
        );
        assert!(found.is_empty(), "an open-namespace key must not be flagged: {found:?}");
    }

    #[test]
    fn malformed_settings_value_produces_an_error_at_origin_and_a_hint_at_usage() {
        // Same reproduction as `comline-core`'s own end-to-end test for
        // this (`core::schema::ir::compiler::settings::enforcement::
        // malformed_settings_value_from_real_parsed_source_points_at_its_
        // true_origin`), but through the real LSP diagnostic-building path
        // - confirms `uri`/`related_information`/the `HINT` diagnostic
        // actually reach the editor, not just that `comline-core`'s
        // `Diagnostic.secondary` field is populated correctly.
        let source = "validator StringBounds for string {\n    min_chars: u32\n    max_chars: u32\n}\n\nstruct Message {\n    @validators = [StringBounds(min_chars=3, max_chars=10)]\n    message: string\n}\n\nsettings {\n    validators.allowed = Tru\n}\n";
        let d = diags(source);

        let errors: Vec<_> =
            d.iter().filter(|x| x.severity == Some(DiagnosticSeverity::ERROR)).collect();
        let hints: Vec<_> = d.iter().filter(|x| x.severity == Some(DiagnosticSeverity::HINT)).collect();
        assert_eq!(errors.len(), 1, "{d:?}");
        assert_eq!(hints.len(), 1, "{d:?}");

        let error = errors[0];
        assert!(error.message.contains("must be a boolean"), "{error:?}");
        assert!(error.related_information.is_some(), "error should link to the usage site: {error:?}");
        assert_eq!(
            error.code,
            Some(NumberOrString::String("settings::malformed-value".to_string())),
            "{error:?}"
        );

        let hint = hints[0];
        assert!(hint.related_information.is_some(), "hint should link back to the real error: {hint:?}");
        assert_eq!(hint.code, error.code, "the hint should share the error's own code: {hint:?}");

        // The settings block is textually after the struct, so its origin
        // span's line must be later than the usage site's - the mix-up
        // this whole change fixes would otherwise put the error where the
        // hint is and vice versa.
        assert!(
            error.range.start.line > hint.range.start.line,
            "the error should land on the settings entry, after the usage site in this source: {d:?}"
        );
    }

    #[test]
    fn forbidden_by_false_is_still_exactly_one_error_diagnostic_with_no_hint() {
        // The sibling case: a correctly-written `= false` forbidding this
        // usage must stay exactly as it was before this change - one error,
        // at the usage site, no secondary `HINT` diagnostic.
        let source = "validator StringBounds for string {\n    min_chars: u32\n    max_chars: u32\n    validate {\n        assert(value.length >= params.min_chars and value.length <= params.max_chars, \"x\")\n    }\n}\n\nstruct Message {\n    @validators = [StringBounds(min_chars=3, max_chars=10)]\n    message: string\n}\n\nsettings {\n    validators.allowed = false\n}\n";
        let d = diags(source);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            d[0].code,
            Some(NumberOrString::String("settings::forbidden".to_string())),
            "{:?}",
            d[0]
        );
    }

    /// The three throws diagnostics core#93 added. This module forwards core's
    /// diagnostics generically rather than naming each one, so nothing here
    /// had to change for them to reach the editor — which is exactly why it is
    /// worth pinning: a future change to that forwarding would silently drop
    /// them, with no other test noticing.
    #[test]
    fn a_bad_throws_clause_reaches_the_editor() {
        for (source, code, needle) in [
            (
                "protocol P {\n    function get(u64) -> string ! Nope;\n}\n",
                "validation::unresolved-throw",
                "no error named 'Nope'",
            ),
            (
                "struct Thing {\n    id: u64\n}\nprotocol P {\n    function get(u64) -> string ! Thing;\n}\n",
                "validation::throw-not-an-error",
                "'Thing' is a struct",
            ),
            (
                "error Denied {\n    message = \"no\"\n}\nprotocol P {\n    function get(u64) -> string ! Denied, Denied;\n}\n",
                "validation::duplicate-throw",
                "more than once",
            ),
        ] {
            let d = diags(source);
            let found = d
                .iter()
                .find(|x| x.code == Some(NumberOrString::String(code.to_string())))
                .unwrap_or_else(|| panic!("expected {code}, got {d:?}"));
            assert_eq!(found.severity, Some(DiagnosticSeverity::ERROR), "{found:?}");
            assert!(found.message.contains(needle), "{found:?}");
            // The help core attaches is the actionable half; this module's
            // other conversion once dropped it, so check it survives.
            assert!(found.message.contains(" — "), "no context/help: {found:?}");
            // Spanned at the function, not the whole file.
            assert!(found.range.end.line > 0 || found.range.end.character > 0, "{found:?}");
        }
    }

    /// `! A, B` is valid now, so it must not be reported as anything.
    #[test]
    fn several_declared_throws_are_clean() {
        let source = "error NotFound {\n    message = \"gone\"\n}\n\
                      error Denied {\n    message = \"no\"\n}\n\
                      protocol P {\n    function get(u64) -> string ! Denied, NotFound;\n}\n";
        assert!(diags(source).is_empty(), "{:?}", diags(source));
    }

    #[test]
    fn deprecated_struct_usage_is_a_warning_with_a_deprecated_tag() {
        // The first diagnostic in this codebase that isn't `Severity::Error`
        // - confirms `lsp_severity`'s non-Error branches and the
        // `DiagnosticTag::DEPRECATED` strikethrough actually reach the LSP
        // diagnostic, not just that `comline-core`'s own `Severity::Warning`
        // is correct in isolation.
        let source = "@deprecated = \"use NewAuthor instead\"\nstruct Author {\n    name: string\n}\n\nstruct Post {\n    author: Author\n}\n";
        let d = diags(source);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::WARNING), "{:?}", d[0]);
        assert_eq!(
            d[0].code,
            Some(NumberOrString::String("validation::deprecated-type-usage".to_string())),
            "{:?}",
            d[0]
        );
        assert_eq!(d[0].tags, Some(vec![DiagnosticTag::DEPRECATED]), "{:?}", d[0]);
        assert!(d[0].message.contains("use NewAuthor instead"), "{:?}", d[0]);
    }

    #[test]
    fn settings_allow_suppresses_the_deprecation_warning_entirely() {
        let source = "@deprecated\nstruct Author {\n    name: string\n}\n\nstruct Post {\n    author: Author\n}\n\nsettings {\n    diagnostics.\"validation::deprecated-type-usage\".level = \"allow\"\n}\n";
        let d = diags(source);
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn settings_error_escalates_the_deprecation_warning_to_a_real_lsp_error() {
        let source = "@deprecated\nstruct Author {\n    name: string\n}\n\nstruct Post {\n    author: Author\n}\n\nsettings {\n    diagnostics.\"validation::deprecated-type-usage\".level = \"error\"\n}\n";
        let d = diags(source);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::ERROR), "{:?}", d[0]);
        assert_eq!(
            d[0].code,
            Some(NumberOrString::String("validation::deprecated-type-usage".to_string())),
            "{:?}",
            d[0]
        );
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

    /// An unknown validator is underlined where it's named, not across the
    /// whole alias it's on.
    #[test]
    fn an_unknown_validator_is_underlined_at_its_name() {
        let messages = project_messages(&[("file:///pkg/src/api.ids", "@validators = [Pos]\ntype FixedNum = string\n")]);
        assert_eq!(messages.len(), 1, "{messages:?}");
        let (message, _, line, character) = &messages[0];
        assert!(message.starts_with("Unknown validator 'Pos'"), "{message}");
        assert_eq!((*line, *character), (0, 15));
    }

    /// An imported validator's arguments are checked in the editor, against
    /// the declaration its `use` brings in - as `comline build` does.
    #[test]
    fn an_imported_validators_arguments_are_checked() {
        let rules = ("file:///pkg/src/rules.ids", "validator AtLeast for u32 {\n    min: u32\n    validate {\n        \
                      assert(value >= params.min, \"{value.name}\")\n    }\n}\n");
        let messages = |call: &str| -> Vec<String> {
            let api = format!("use rules::AtLeast\n\nstruct S {{\n    @validators = [{call}]\n    n: u32\n}}\n");
            project_messages(&[("file:///pkg/src/api.ids", &api), rules]).into_iter().map(|m| m.0).collect()
        };
        let only = |call: &str, expected: &str| {
            let got = messages(call);
            assert!(got.len() == 1 && got[0].starts_with(expected), "{call}: {got:?}");
        };
        only("AtLeast", "validator 'AtLeast' is missing argument 'min'");
        only("AtLeast(mni = 1)", "validator 'AtLeast' has no argument 'mni' - did you mean 'min'?");
        assert!(messages("AtLeast(min = 1)").is_empty());
    }

    const TYPES: (&str, &str) = ("file:///pkg/src/types.ids", "struct Message {\n    text: string\n}\n");

    /// A validator and an alias from another file: the editor checks the
    /// field against the validator's `for` the way `comline build` does,
    /// resolving the alias first - core alone would skip both as imported.
    #[test]
    fn an_imported_validator_is_checked_against_the_field_type() {
        const RULES: (&str, &str) = (
            "file:///pkg/src/rules.ids",
            "type Count = u32\ntype Name = string\n\n\
             validator NonEmpty for string {\n    validate {\n        assert(value.length > 0, \"{value.name} is empty\")\n    }\n}\n",
        );
        let bad = project_messages(&[
            ("file:///pkg/src/api.ids", "use rules::{Count, NonEmpty}\n\nstruct S {\n    @validators = [NonEmpty()]\n    n: Count\n}\n"),
            RULES,
        ]);
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert_eq!(
            bad[0].0,
            "validator 'NonEmpty' applies to `string`, not `Count` (`u32`) — Struct 'S', field 'n'"
        );
        assert_eq!(
            bad[0].1,
            Some(NumberOrString::String("validation::validator-target-mismatch".to_string()))
        );

        let ok = project_messages(&[
            ("file:///pkg/src/api.ids", "use rules::{Name, NonEmpty}\n\nstruct S {\n    @validators = [NonEmpty()]\n    n: Name\n}\n"),
            RULES,
        ]);
        assert!(ok.is_empty(), "{ok:?}");
    }

    const BOUNDS: (&str, &str) = (
        "file:///pkg/src/rules.ids",
        "validator Bounds for string {\n    validate {\n        assert(value.length > 0, \"{value.name} is empty\")\n    }\n}\n\
         type Name = string\n",
    );

    /// A validator declared in another file but never imported is a missing
    /// `use` - and only that. Finding it project-wide anyway once also checked
    /// its `for` against the alias, an error about a declaration not in scope.
    #[test]
    fn a_validator_the_file_never_imports_is_unknown_and_nothing_more() {
        let messages: Vec<String> = project_messages(&[
            ("file:///pkg/src/api.ids", "@validators = [Bounds()]\ntype FixedNum = u8[2]\n"),
            BOUNDS,
        ])
        .into_iter()
        .map(|m| m.0)
        .collect();
        assert_eq!(
            messages,
            ["`Bounds` is declared in `rules.ids` but not imported here — add `use rules::Bounds`"]
        );

        // Imported, it's found - and then its target is what's wrong.
        let messages: Vec<String> = project_messages(&[
            ("file:///pkg/src/api.ids", "use rules::Bounds\n\n@validators = [Bounds()]\ntype FixedNum = u8[2]\n"),
            BOUNDS,
        ])
        .into_iter()
        .map(|m| m.0)
        .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("validator 'Bounds' applies to `string`, not `u8[2]`"),
            "{messages:?}"
        );
    }

    /// The editor's own case: std's `StringBounds` is in the project (std's
    /// files always are), but a file that doesn't `use` it doesn't have it.
    #[test]
    fn std_validators_are_only_in_scope_through_a_use() {
        let in_package = |text: &str| {
            let mut files = vec![crate::analysis::source::SourceFile::local(
                Url::parse("file:///pkg/src/none.ids").unwrap(),
                text.to_string(),
            )];
            files.extend(crate::analysis::stdlib::files(&crate::analysis::stdlib::root()));
            let project = Project::new(files.iter());
            project_diagnostics(&project, 0).into_iter().map(|d| d.message).collect::<Vec<_>>()
        };

        let unimported = in_package("@validators = [StringBounds()]\ntype FixedNum = u8[2]\n");
        assert_eq!(
            unimported,
            ["`StringBounds` is declared in `validators.ids` but not imported here — \
              add `use std::validators::StringBounds`"]
        );

        let imported = in_package(
            "use std::validators::StringBounds\n\n@validators = [StringBounds()]\ntype FixedNum = u8[2]\n",
        );
        assert_eq!(imported.len(), 1, "{imported:?}");
        assert!(
            imported[0].starts_with("validator 'StringBounds' applies to `string`, not `u8[2]`"),
            "{imported:?}"
        );
    }

    /// An alias from another file, never imported, isn't resolved to judge a
    /// validator on it - the unknown type is the one error.
    #[test]
    fn an_alias_the_file_never_imports_is_not_resolved() {
        let messages: Vec<String> = project_messages(&[
            (
                "file:///pkg/src/api.ids",
                "validator Positive for u32 {\n    validate {\n        assert(value > 0, \"{value.name}\")\n    }\n}\n\
                 struct S {\n    @validators = [Positive()]\n    n: Name\n}\n",
            ),
            BOUNDS,
        ])
        .into_iter()
        .map(|m| m.0)
        .collect();
        assert!(messages.iter().all(|m| !m.contains("applies to")), "{messages:?}");
    }

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
    fn import_codes_use_the_namespaced_convention() {
        // Explicit value assertions (not just "the constant is referenced
        // correctly" via the tests below) so an accidental revert to the
        // old bare-string codes is actually caught.
        assert_eq!(MISSING_IMPORT, "import::missing");
        assert_eq!(UNRESOLVED_IMPORT, "import::unresolved");
        assert_eq!(UNVERIFIED_IMPORT, "import::unverified");
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

    const MANIFEST_URI: &str = "file:///pkg/config.idp";

    #[test]
    fn a_package_forbidden_annotation_surfaces_as_a_live_diagnostic() {
        // `.idp` has no dotted-key sugar (that's `.ids`-only) - a nested
        // key needs real nested braces here.
        let manifest = "congregation pkg\nspecification_version = 1\n\
            settings = {\n    protocol = {\n        annotations = {\n            \
            framing = {\n                allowed = false\n            }\n        }\n    }\n}\n";
        let chat = "@framing = \"jsonrpc\"\nprotocol Chat {\n    function ping() -> string;\n}\n";

        let found = project_messages(&[(MANIFEST_URI, manifest), ("file:///pkg/src/chat.ids", chat)]);
        assert!(
            found.iter().any(|(msg, ..)| msg.contains("framing")),
            "expected a forbidden-annotation diagnostic, got {found:?}"
        );
    }

    #[test]
    fn editing_the_manifest_to_allow_it_clears_the_diagnostic() {
        let manifest = "congregation pkg\nspecification_version = 1\n\
            settings = {\n    protocol = {\n        annotations = {\n            \
            framing = {\n                allowed = true\n            }\n        }\n    }\n}\n";
        let chat = "@framing = \"jsonrpc\"\nprotocol Chat {\n    function ping() -> string;\n}\n";

        let found = project_messages(&[(MANIFEST_URI, manifest), ("file:///pkg/src/chat.ids", chat)]);
        assert!(
            !found.iter().any(|(msg, ..)| msg.contains("framing")),
            "expected no forbidden-annotation diagnostic once the manifest allows it, got {found:?}"
        );
    }

    #[test]
    fn no_manifest_means_no_package_settings_to_enforce() {
        // Mirrors `all_diagnostics`'s "project of one" path - no config.idp
        // in view, so nothing can forbid anything.
        let found = project_messages(&[("file:///pkg/src/chat.ids", TYPES.1)]);
        assert!(found.is_empty(), "{found:?}");
    }
}

/// The duplicate-declaration error used to land at 0:0 - `FrozenUnit::Error`
/// carried no span, so the editor had nothing to point at and fell back to
/// the top of the file. Core now spans both declarations; these cover the
/// narrowing from core's declaration-wide spans down to the two names.
#[cfg(test)]
mod duplicate_declaration_tests {
    use super::*;
    use crate::parser;

    /// The screenshot's file: a struct, an error and a settings preset all
    /// called `Test`.
    const THREE_TESTS: &str = "struct Test {\n    a: string\n}\n\n\
                               error Test {\n    message = \"\"\n}\n\n\
                               /// Restricting some stuff\n\
                               settings Test {\n    validators.allowed = true\n}\n";

    fn diagnostics(source: &str) -> Vec<Diagnostic> {
        let r = parser::parse(source).unwrap();
        all_diagnostics(source, &r.errors, r.document.as_ref())
    }

    /// The range `source` quotes for an LSP range.
    fn quoted<'a>(source: &'a str, range: &lsp_types::Range) -> &'a str {
        let start = crate::util::position_to_offset(source, range.start).unwrap();
        let end = crate::util::position_to_offset(source, range.end).unwrap();
        &source[start..end]
    }

    #[test]
    fn both_colliding_names_are_underlined_and_nothing_else() {
        let found = diagnostics(THREE_TESTS);
        assert_eq!(found.len(), 2, "one error, one linked hint: {found:?}");

        let error = found.iter().find(|d| d.severity == Some(DiagnosticSeverity::ERROR)).unwrap();
        let hint = found.iter().find(|d| d.severity == Some(DiagnosticSeverity::HINT)).unwrap();

        // Both land on the name, not on the whole declaration and not at 0:0.
        assert_eq!(quoted(THREE_TESTS, &error.range), "Test");
        assert_eq!(quoted(THREE_TESTS, &hint.range), "Test");
        assert_ne!(error.range, hint.range, "two different declarations");
        assert_eq!(error.range.start.line, 4, "the error, which collided");
        assert_eq!(hint.range.start.line, 0, "the struct, which held the name");

        // And they point at each other.
        assert_eq!(
            error.related_information.as_ref().unwrap()[0].location.range,
            hint.range,
        );
        assert_eq!(
            hint.related_information.as_ref().unwrap()[0].location.range,
            error.range,
        );
        assert_eq!(error.code, Some(NumberOrString::String(DUPLICATE_DECLARATION.to_string())));

        // The hint sends you to the duplicate, not to a settings entry -
        // that wording was true of the only two-site diagnostic there was
        // when it was written.
        assert!(hint.message.ends_with("see the error at the duplicate"), "{}", hint.message);
    }

    /// A `settings` preset shares no namespace with a declaration, so the
    /// third `Test` in the file is not part of the collision.
    #[test]
    fn the_settings_preset_is_left_alone() {
        for d in diagnostics(THREE_TESTS) {
            assert!(d.range.start.line < 9, "the settings block was flagged: {d:?}");
        }
    }

    #[test]
    fn the_message_names_the_kinds() {
        let found = diagnostics(THREE_TESTS);
        let error = found.iter().find(|d| d.severity == Some(DiagnosticSeverity::ERROR)).unwrap();
        // Qualified by the schema's own namespace, which here is the
        // `schema.ids` the project-of-one path names the file.
        assert!(error.message.contains("error 'schema::Test'"), "{}", error.message);
        assert!(error.message.contains("rename this error"), "{}", error.message);
        assert_eq!(
            error.related_information.as_ref().unwrap()[0].message,
            "struct 'schema::Test' declared here",
        );
    }

    /// A duplicate `settings` block gets the same two-site treatment, and
    /// the same narrowing.
    #[test]
    fn a_duplicate_settings_block_underlines_both_names() {
        let source = "settings Strict {\n    validators.allowed = true\n}\n\n\
                      settings Strict {\n    validators.allowed = false\n}\n";
        let found = diagnostics(source);
        let error = found
            .iter()
            .find(|d| d.code == Some(NumberOrString::String(DUPLICATE_BLOCK.to_string())))
            .expect("a duplicate block is reported");
        assert_eq!(quoted(source, &error.range), "Strict");
        let hint = error.related_information.as_ref().unwrap();
        assert_eq!(quoted(source, &hint[0].location.range), "Strict");
        assert_ne!(error.range, hint[0].location.range);
    }

    /// A schema namespace is `src`-relative and package-name-free
    /// throughout (see `imports::namespace_of`), so `src/chat.ids` in
    /// package `app` is `chat` - the same string `comline build` puts in
    /// the schema's own `FrozenUnit::Namespace`, and the same one you'd
    /// write in a `use`.
    #[test]
    fn a_known_namespace_qualifies_the_name() {
        use crate::analysis::source::SourceFile;
        let chat = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = [SourceFile::local(
            Url::parse("file:///pkg/config.idp").unwrap(),
            "congregation app\nspecification_version = 1\n".to_string(),
        )];
        let project = Project::with_active(&chat, THREE_TESTS, &others);
        let doc = project.index_of(&chat).unwrap();

        let found = project_diagnostics(&project, doc);
        let error = found.iter().find(|d| d.severity == Some(DiagnosticSeverity::ERROR)).unwrap();
        assert!(error.message.contains("'chat::Test'"), "{}", error.message);
    }
}

/// Core refuses a schema whose own filename is an IDL keyword or a name a
/// target language's module system has spoken for. That check lives on the
/// build path, which this server doesn't run, so it is mirrored here -
/// otherwise the editor stays quiet about a file that cannot build at all.
#[cfg(test)]
mod reserved_schema_name_tests {
    use super::*;
    use crate::analysis::source::SourceFile;

    fn diagnostics_for(path: &str) -> Vec<Diagnostic> {
        let uri = Url::parse(path).unwrap();
        let project = Project::with_active(&uri, "struct Thing {\n    id: u64\n}\n", &[] as &[SourceFile]);
        let doc = project.index_of(&uri).unwrap();
        project_diagnostics(&project, doc)
    }

    fn reserved(path: &str) -> Option<Diagnostic> {
        diagnostics_for(path)
            .into_iter()
            .find(|d| d.code == Some(NumberOrString::String(RESERVED_SCHEMA_NAME.to_string())))
    }

    #[test]
    fn a_target_module_name_is_flagged_at_the_top_of_the_file() {
        let d = reserved("file:///pkg/src/main.ids").expect("main.ids is reserved");
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.range, Range::default(), "the file's own name is what's wrong");
        assert!(d.message.contains("'main'"), "{}", d.message);
        assert!(d.message.contains("module name in generated code"), "{}", d.message);
    }

    #[test]
    fn a_keyword_gets_the_other_explanation() {
        let d = reserved("file:///pkg/src/self.ids").expect("self.ids is reserved");
        assert!(d.message.contains("keyword of the schema language"), "{}", d.message);
    }

    /// A directory segment is as much a module name as the filename is.
    #[test]
    fn a_reserved_directory_segment_is_flagged() {
        assert!(reserved("file:///pkg/src/main/foo.ids").is_some());
    }

    #[test]
    fn an_ordinary_name_is_not_flagged() {
        assert!(reserved("file:///pkg/src/greeting.ids").is_none());
    }
}
