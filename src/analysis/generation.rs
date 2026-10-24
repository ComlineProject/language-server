//! What the package's declared languages can generate, from each
//! generator's own `SUPPORT` table (`comline_codegen::features`), at the
//! declared version: a warning at each use of a feature a declared
//! language's generator refuses, a note at one it generates without, the
//! summary hovering the language in `config.idp` shows, and the problems
//! with a declaration itself that `comline build` refuses.
//!
//! The tables are the generators' own, proven by their conformance suite;
//! nothing is generated here. A declared language this build has no
//! generator for gets no feature warnings; its declaration gets an error,
//! as `comline build` refuses it.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use comline_codegen::features::{self, Feature, Options, State, Support};
use comline_codegen::versions::LangVersion;
use comline_codegen::{Registry, Resolved, Unresolved};
use comline_core::diagnostics::{Diagnostic as CoreDiagnostic, DiagnosticCode, Severity};
use comline_core::schema::ir::compiler::settings::severity::resolve_diagnostic_overrides;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use comline_core::settings::{SettingsDict, SettingsSpans};
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};

use crate::analysis::source::DeclaredLanguage;
use crate::util::byte_range_to_lsp_range;

/// A use of a feature the declared language's generator refuses.
pub const UNSUPPORTED_FEATURE: &str = "generation::unsupported-feature";
/// A use of a feature the declared language's generator generates without.
pub const IGNORED_FEATURE: &str = "generation::ignored-feature";
/// A declaration no generator handles: an unknown language, a version that
/// isn't one, or one below the generator's floor.
pub const UNRESOLVED_TARGET: &str = "generation::unresolved-target";
/// A declaration below what a dependency declares for the same language.
pub const BELOW_DEPENDENCY: &str = "generation::below-dependency";
/// A declaration past what its generator has verified - the same code
/// `comline build` warns under, and settings can raise the same way.
pub const UNVERIFIED_VERSION: DiagnosticCode = DiagnosticCode::warning("generation::unverified-version");

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = Registry::new();
        comline_codegen_rust::register(&mut registry);
        comline_codegen_typescript::register(&mut registry);
        registry
    })
}

/// The generator for `language#version`, as `comline build` resolves it.
pub fn resolve(language: &str, version: &str) -> Result<Resolved, Unresolved> {
    registry().resolve(language, version)
}

/// The diagnostics for one schema's `units` (its namespace already among
/// them, or not - it isn't read), for every language in `languages`.
pub fn feature_diagnostics(source: &str, units: &[FrozenUnit], languages: &[DeclaredLanguage]) -> Vec<Diagnostic> {
    // Only the schema's own features: the request options (`lib` mode,
    // `external_std`, ...) live in `comline.toml`, not here.
    let uses = features::used(&[(String::new(), units.to_vec())], &Options::default());
    let mut diagnostics = Vec::new();

    for declared in languages {
        let Ok(resolved) = resolve(&declared.language, &declared.version) else { continue };
        let target = format!("{}#{}", declared.language, declared.version);
        for u in &uses {
            let Some(span) = u.span else { continue };
            let s = resolved.support(u.feature);
            let (severity, code, message) = match s.state {
                State::Unsupported => (
                    DiagnosticSeverity::WARNING,
                    UNSUPPORTED_FEATURE,
                    format!(
                        "{} can't be generated for `{target}`{} - `comline generate` refuses this schema for it",
                        u.feature.title(),
                        detail(&declared.language, &s)
                    ),
                ),
                State::Ignored => (
                    DiagnosticSeverity::INFORMATION,
                    IGNORED_FEATURE,
                    format!("`{target}` generates this without its {}{}", u.feature.title(), detail(&declared.language, &s)),
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

/// `: note (tracking)`, or as much of it as there is; for a feature gated
/// above the declared version, the version to declare.
fn detail(language: &str, s: &Support) -> String {
    let mut out = String::new();
    if let Some(note) = s.note {
        out.push_str(&format!(": {note}"));
    }
    if let (State::Unsupported, Some(since)) = (s.state, s.since) {
        out.push_str(&format!(" - declare `{language}#{}` or later", since.version));
    }
    if let Some(tracking) = s.tracking {
        out.push_str(&format!(" ({tracking} tracks it)"));
    }
    out
}

/// What `comline build` refuses or warns about in `manifest`'s
/// `code_generation.languages` entries, at each entry: no generator handles
/// it, a dependency declares more (`dependencies` holds each dependency's
/// declarations, by name), or it's past what its generator has verified -
/// that last one with the package's diagnostics `settings` applied.
pub fn declaration_diagnostics(
    manifest: &str,
    languages: &[DeclaredLanguage],
    settings: &SettingsDict,
    dependencies: &[(String, DeclaredLanguage)],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for declared in languages {
        let target = format!("{}#{}", declared.language, declared.version);
        let range = entry_range(manifest, &target);
        let mut push = |severity, code: &str, message: String| {
            diagnostics.push(Diagnostic {
                range: byte_range_to_lsp_range(manifest, range.0, range.1),
                severity: Some(severity),
                code: Some(NumberOrString::String(code.to_string())),
                source: Some("comline".to_string()),
                message,
                ..Default::default()
            })
        };
        let resolved = match resolve(&declared.language, &declared.version) {
            Ok(resolved) => resolved,
            Err(Unresolved::UnknownLanguage { language }) => {
                push(
                    DiagnosticSeverity::ERROR,
                    UNRESOLVED_TARGET,
                    format!("this build of comline has no {language} generator - `comline build` can't check `{target}`, and refuses it"),
                );
                continue;
            }
            Err(e) => {
                push(DiagnosticSeverity::ERROR, UNRESOLVED_TARGET, e.to_string());
                continue;
            }
        };
        let highest = dependencies
            .iter()
            .filter(|(_, d)| d.language.eq_ignore_ascii_case(&declared.language))
            .filter_map(|(name, d)| Some((name, d, LangVersion::parse(&d.version)?)))
            .max_by(|a, b| a.2.cmp(&b.2));
        if let Some((name, d, minimum)) = highest {
            if resolved.version < minimum {
                push(
                    DiagnosticSeverity::ERROR,
                    BELOW_DEPENDENCY,
                    format!(
                        "dependency `{name}` declares `{}#{}`, and its generated code needs that toolchain - \
                         declare `{}#{}` or later",
                        d.language, d.version, declared.language, d.version
                    ),
                );
            }
        }
        if !resolved.is_verified() {
            let versions = resolved.generator.versions;
            let message = format!(
                "past what the {} generator has verified (up to {}): it generates as for {}, and the output \
                 may not build on that toolchain",
                declared.language,
                versions.verified_up_to,
                resolved.boundary()
            );
            let (warnings, fatal) = resolve_diagnostic_overrides(
                &[],
                vec![CoreDiagnostic::new(UNVERIFIED_VERSION, message.clone())],
                settings,
                &SettingsSpans::default(),
            );
            let severity = match (fatal.first(), warnings.first()) {
                (Some(_), _) => Some(DiagnosticSeverity::ERROR),
                (None, Some(w)) => Some(match w.effective_severity() {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                    Severity::Info => DiagnosticSeverity::INFORMATION,
                    Severity::Hint => DiagnosticSeverity::HINT,
                }),
                (None, None) => None,
            };
            if let Some(severity) = severity {
                push(severity, UNVERIFIED_VERSION.id, message);
            }
        }
    }
    diagnostics
}

/// The range of `target` (`rust#1.85.0`) in the manifest: its first
/// occurrence after `languages`.
fn entry_range(manifest: &str, target: &str) -> (usize, usize) {
    let from = manifest.find("languages").unwrap_or(0);
    manifest[from..]
        .find(target)
        .map(|i| (from + i, from + i + target.len()))
        .unwrap_or((from, from))
}

/// Markdown summarising the generator for `language#version`: the versions
/// it handles, then what it doesn't support, what it only partly does, and
/// what it ignores at that version - everything else is supported. Why it
/// can't generate it, when it can't; `None` when this build has no such
/// generator.
pub fn summary(language: &str, version: &str) -> Option<String> {
    let resolved = match resolve(language, version) {
        Ok(resolved) => resolved,
        Err(Unresolved::UnknownLanguage { .. }) => return None,
        Err(e) => return Some(format!("**`{language}#{version}`**: {e}.")),
    };
    let mut by_state: BTreeMap<u8, Vec<String>> = BTreeMap::new();
    let mut supported = 0;
    for &feature in Feature::ALL {
        let s = resolved.support(feature);
        let rank = match s.state {
            State::Unsupported => 0,
            State::Partial => 1,
            State::Ignored => 2,
            State::Supported => {
                supported += 1;
                continue;
            }
        };
        by_state.entry(rank).or_default().push(format!("- {}{}", feature.title(), detail(language, &s)));
    }

    let versions = resolved.generator.versions;
    let mut out = format!("**`{language}#{version}`** supports {supported} of {} features.", Feature::ALL.len());
    out.push_str(&format!(
        "\n\nThe {language} generator handles {} or later, verified up to {}{}.",
        versions.floor,
        versions.verified_up_to,
        if versions.forward_compatible { "; newer toolchains build its output too" } else { "" }
    ));
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
        let found = diagnostics("        rust#1.85.0 = {}\n", UNION);
        assert_eq!(found.len(), 1, "{found:?}");
        let (code, message, line, character) = &found[0];
        assert_eq!(code, super::UNSUPPORTED_FEATURE);
        assert!(message.starts_with("unions can't be generated for `rust#1.85.0`: "), "{message}");
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
                      assert(value.length > 0, f\"{value.name} is empty\")\n    }\n}\n\n\
                      @validators = [NonEmpty]\ntype Name = string\n";
        let found = diagnostics("        typescript#5.0 = {}\n        rust#1.85.0 = {}\n", schema);
        assert_eq!(found.len(), 1, "{found:?}");
        let (code, message, line, character) = &found[0];
        assert_eq!(code, super::IGNORED_FEATURE);
        assert!(message.starts_with("`typescript#5.0` generates this without its validators: "), "{message}");
        assert_eq!((*line, *character), (6, 15), "at the validator's name");
    }

    #[test]
    fn the_summary_lists_what_isnt_fully_supported() {
        let summary = super::summary("rust", "1.85.0").unwrap();
        assert!(summary.starts_with("**`rust#1.85.0`** supports "), "{summary}");
        assert!(summary.contains("Not supported (generation refuses a schema using these):\n- "), "{summary}");
        assert!(summary.contains("\n- unions: "), "{summary}");
        assert_eq!(super::summary("python", "3.11.0"), None);
    }

    #[test]
    fn the_summary_names_the_generators_range_or_why_it_has_none() {
        let summary = super::summary("rust", "1.88.0").unwrap();
        assert!(summary.contains("The rust generator handles 1.85.0 or later, verified up to 1.99"), "{summary}");
        let below = super::summary("rust", "1.70.0").unwrap();
        assert!(below.contains("it needs `rust#1.85.0` or later"), "{below}");
    }

    /// `declaration_diagnostics` for `languages`, with `rest` after the
    /// languages block and `dependencies` declaring.
    fn declarations(languages: &str, rest: &str, dependencies: &[(&str, &str)]) -> Vec<(String, String, String)> {
        let manifest = format!(
            "congregation app\nspecification_version = 1\n\ncode_generation = {{\n    languages = {{\n{languages}    }}\n}}\n{rest}"
        );
        let languages = crate::analysis::source::declared_languages(&manifest);
        let settings = crate::analysis::source::declared_settings(&manifest);
        let dependencies: Vec<_> = dependencies
            .iter()
            .map(|(name, target)| {
                let (language, version) = target.split_once('#').unwrap();
                (name.to_string(), crate::analysis::source::DeclaredLanguage { language: language.into(), version: version.into() })
            })
            .collect();
        super::declaration_diagnostics(&manifest, &languages, &settings, &dependencies)
            .into_iter()
            .map(|d| {
                let code = match d.code {
                    Some(lsp_types::NumberOrString::String(code)) => code,
                    _ => String::new(),
                };
                let at = manifest
                    .lines()
                    .nth(d.range.start.line as usize)
                    .map(|l| l[d.range.start.character as usize..d.range.end.character as usize].to_string())
                    .unwrap_or_default();
                (code, format!("{:?}", d.severity.unwrap()), format!("{at}: {}", d.message))
            })
            .collect()
    }

    #[test]
    fn a_declaration_comline_build_refuses_is_an_error_at_the_entry() {
        let found = declarations("        rust#1.70.0 = {}\n        python#3.11 = {}\n", "", &[]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, super::UNRESOLVED_TARGET);
        assert_eq!(found[0].1, "Error");
        assert!(found[0].2.starts_with("rust#1.70.0: "), "{found:?}");
        assert!(found[0].2.contains("it needs `rust#1.85.0` or later"), "{found:?}");
        assert!(found[1].2.contains("has no python generator"), "{found:?}");
        assert_eq!(declarations("        rust#1.85.0 = {}\n        typescript#5.4 = {}\n", "", &[]), []);
    }

    #[test]
    fn a_declaration_below_a_dependencys_is_an_error() {
        let found = declarations("        rust#1.85.0 = {}\n", "", &[("shared", "rust#1.90.0"), ("other", "rust#1.88.0")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, super::BELOW_DEPENDENCY);
        assert!(found[0].2.contains("dependency `shared` declares `rust#1.90.0`"), "{found:?}");
        assert_eq!(declarations("        rust#1.90.0 = {}\n", "", &[("shared", "rust#1.90.0")]), []);
    }

    #[test]
    fn past_the_verified_range_is_a_warning_settings_can_raise() {
        let found = declarations("        typescript#6.0 = {}\n", "", &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, super::UNVERIFIED_VERSION.id);
        assert_eq!(found[0].1, "Warning");
        assert!(found[0].2.contains("past what the typescript generator has verified (up to 5.9)"), "{found:?}");

        let raised = "settings = {\n    diagnostics = {\n        generation::unverified-version = {\n            level = \"error\"\n        }\n    }\n}\n";
        assert_eq!(declarations("        typescript#6.0 = {}\n", raised, &[])[0].1, "Error");
        let allowed = raised.replace("\"error\"", "\"allow\"");
        assert_eq!(declarations("        typescript#6.0 = {}\n", &allowed, &[]), []);
        // Rust promises newer compilers build older code: verified.
        assert_eq!(declarations("        rust#2.5 = {}\n", "", &[]), []);
    }
}
