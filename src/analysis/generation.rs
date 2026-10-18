//! What the package's declared languages can generate, from each
//! generator's own `SUPPORT` table (`comline_codegen::features`): a warning
//! at each use of a feature a declared language's generator refuses, a note
//! at one it generates without, and the summary hovering the language in
//! `config.idp` shows.
//!
//! The tables are the generators' own, proven by their conformance suite;
//! nothing is generated here. A declared language this build has no
//! generator for gets nothing - `comline generate` reports that one.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use comline_codegen::features::{self, Feature, Options, State, Support, SupportTable};
use comline_codegen::Registry;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

use crate::analysis::source::DeclaredLanguage;
use crate::util::byte_range_to_lsp_range;

/// A use of a feature the declared language's generator refuses.
pub const UNSUPPORTED_FEATURE: &str = "generation::unsupported-feature";
/// A use of a feature the declared language's generator generates without.
pub const IGNORED_FEATURE: &str = "generation::ignored-feature";

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = Registry::new();
        comline_codegen_rust::register(&mut registry);
        comline_codegen_typescript::register(&mut registry);
        registry
    })
}

/// What the generator for `language` at `version` supports, if this build
/// has one.
pub fn support(language: &str, version: &str) -> Option<SupportTable> {
    registry().support(language, version)
}

/// The diagnostics for one schema's `units` (its namespace already among
/// them, or not - it isn't read), for every language in `languages`.
pub fn feature_diagnostics(source: &str, units: &[FrozenUnit], languages: &[DeclaredLanguage]) -> Vec<Diagnostic> {
    // Only the schema's own features: the request options (`lib` mode,
    // `external_std`, ...) live in `comline.toml`, not here.
    let uses = features::used(&[(String::new(), units.to_vec())], &Options::default());
    let mut diagnostics = Vec::new();

    for declared in languages {
        let Some(table) = support(&declared.language, &declared.version) else { continue };
        let target = format!("{}#{}", declared.language, declared.version);
        for u in &uses {
            let Some(span) = u.span else { continue };
            let s = features::support_for(table, u.feature);
            let (severity, code, message) = match s.state {
                State::Unsupported => (
                    DiagnosticSeverity::WARNING,
                    UNSUPPORTED_FEATURE,
                    format!("{} can't be generated for `{target}`{} - `comline generate` refuses this schema for it", u.feature.title(), detail(&s)),
                ),
                State::Ignored => (
                    DiagnosticSeverity::INFORMATION,
                    IGNORED_FEATURE,
                    format!("`{target}` generates this without its {}{}", u.feature.title(), detail(&s)),
                ),
                State::Supported | State::Partial => continue,
            };
            diagnostics.push(Diagnostic {
                range: byte_range_to_lsp_range(source, span.0, span.1),
                severity: Some(severity),
                code: Some(NumberOrString::String(code.to_string())),
                code_description: None,
                source: Some("comline".to_string()),
                message,
                related_information: None,
                tags: None,
                data: None,
            });
        }
    }
    diagnostics
}

/// `: note (tracking)`, or as much of it as there is.
fn detail(s: &Support) -> String {
    let mut out = String::new();
    if let Some(note) = s.note {
        out.push_str(&format!(": {note}"));
    }
    if let Some(tracking) = s.tracking {
        out.push_str(&format!(" ({tracking} tracks it)"));
    }
    out
}

/// Markdown summarising what the generator for `language#version`
/// supports: what it doesn't, what it only partly does, what it ignores -
/// everything else is supported. `None` when this build has no such
/// generator.
pub fn summary(language: &str, version: &str) -> Option<String> {
    let table = support(language, version)?;
    let mut by_state: BTreeMap<u8, Vec<String>> = BTreeMap::new();
    let mut supported = 0;
    for &feature in Feature::ALL {
        let s = features::support_for(table, feature);
        let rank = match s.state {
            State::Unsupported => 0,
            State::Partial => 1,
            State::Ignored => 2,
            State::Supported => {
                supported += 1;
                continue;
            }
        };
        by_state.entry(rank).or_default().push(format!("- {}{}", feature.title(), detail(&s)));
    }

    let mut out = format!("**`{language}#{version}`** supports {supported} of {} features.", Feature::ALL.len());
    for (rank, lines) in by_state {
        let heading = match rank {
            0 => "Not supported (generation refuses a schema using these):",
            1 => "Partly supported:",
            _ => "Ignored (generated without them):",
        };
        out.push_str(&format!("\n\n{heading}\n{}", lines.join("\n")));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use lsp_types::Url;

    use crate::analysis::diagnostics::project_diagnostics;
    use crate::analysis::project::Project;

    /// The package's diagnostics for `schema`, with `languages` declared.
    fn diagnostics(languages: &str, schema: &str) -> Vec<(String, String, u32, u32)> {
        let manifest = format!(
            "congregation app\nspecification_version = 1\n\ncode_generation = {{\n    languages = {{\n{languages}    }}\n}}\n"
        );
        let files = [
            (Url::parse("file:///pkg/config.idp").unwrap(), manifest),
            (Url::parse("file:///pkg/src/api.ids").unwrap(), schema.to_string()),
        ];
        let project = Project::new(files.iter());
        let doc = project.docs.iter().position(|d| d.uri.path().ends_with("api.ids")).unwrap();
        project_diagnostics(&project, doc)
            .into_iter()
            .filter_map(|d| match d.code {
                Some(lsp_types::NumberOrString::String(code)) if code.starts_with("generation::") => {
                    Some((code, d.message, d.range.start.line, d.range.start.character))
                }
                _ => None,
            })
            .collect()
    }

    const UNION: &str = "struct Lookup {\n    key: union(string u32)\n}\n";

    #[test]
    fn a_feature_a_declared_language_refuses_is_a_warning_at_the_use() {
        let found = diagnostics("        rust#1.70.0 = {}\n", UNION);
        assert_eq!(found.len(), 1, "{found:?}");
        let (code, message, line, character) = &found[0];
        assert_eq!(code, super::UNSUPPORTED_FEATURE);
        assert!(message.starts_with("unions can't be generated for `rust#1.70.0`: "), "{message}");
        assert_eq!((*line, *character), (1, 4), "at the field");
    }

    #[test]
    fn a_language_that_supports_it_says_nothing() {
        assert_eq!(diagnostics("        typescript#5.0 = {}\n", UNION), []);
        // Nor does a language this build has no generator for.
        assert_eq!(diagnostics("        python#3.11.0 = {}\n", UNION), []);
        // Nor anything, without a manifest declaring languages.
        assert_eq!(diagnostics("", UNION), []);
    }

    #[test]
    fn a_feature_a_declared_language_ignores_is_a_note() {
        let schema = "validator NonEmpty for string {\n    validate {\n        \
                      assert(value.length > 0, \"{value.name} is empty\")\n    }\n}\n\n\
                      @validators = [NonEmpty]\ntype Name = string\n";
        let found = diagnostics("        typescript#5.0 = {}\n        rust#1.70.0 = {}\n", schema);
        assert_eq!(found.len(), 1, "{found:?}");
        let (code, message, line, character) = &found[0];
        assert_eq!(code, super::IGNORED_FEATURE);
        assert!(message.starts_with("`typescript#5.0` generates this without its validators: "), "{message}");
        assert_eq!((*line, *character), (6, 15), "at the validator's name");
    }

    #[test]
    fn the_summary_lists_what_isnt_fully_supported() {
        let summary = super::summary("rust", "1.70.0").unwrap();
        assert!(summary.starts_with("**`rust#1.70.0`** supports "), "{summary}");
        assert!(summary.contains("Not supported (generation refuses a schema using these):\n- "), "{summary}");
        assert!(summary.contains("\n- unions: "), "{summary}");
        assert_eq!(super::summary("python", "3.11.0"), None);
    }
}
