//! What the package's declared languages can generate, from each
//! generator's own `SUPPORT` table (`comline_codegen::features`), at the
//! declared version: a warning at each use of a feature a declared
//! language's generator refuses or generates without, the summary hovering
//! the language in `config.idp` shows, and the problems with a declaration
//! itself that `comline build` refuses.
//!
//! It also shows what `comline generation` says about the package's
//! connection beyond generating (`comline_codegen::checks`): the protocols
//! that disagree on one, and the bare method names that clash. The rest of
//! those checks are about one target, and `comline.toml` shows them
//! (`comline_toml`), over the same schemas ([`request`]).
//!
//! The tables are the generators' own, proven by their conformance suite;
//! nothing is generated here. A declared language this build has no
//! generator for gets no feature warnings; its declaration gets an error,
//! as `comline build` refuses it.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use comline_codegen::docs::{self, Topic};
use comline_codegen::features::{self, Feature, Options, State, Support};
use comline_codegen::protocol::EncodingMapping;
use comline_codegen::versions::LangVersion;
use comline_codegen::{
    checks, GenerateRequest, Mode, PackageMeta, Registry, Resolved, TargetLanguage, Unresolved,
};
use comline_core::diagnostics::{Diagnostic as CoreDiagnostic, DiagnosticCode};
use comline_core::package::config::ir::frozen::{self as config, FrozenUnit as ConfigUnit};
use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
use comline_core::schema::ir::compiler::settings::severity::resolve_diagnostic_overrides;
use comline_core::schema::ir::compiler::Compile;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use comline_core::schema::ir::package_wire::apply_package_wire;
use comline_core::settings::{SettingsDict, SettingsSpans};
use lsp_types::{Diagnostic, DiagnosticSeverity, SymbolKind};

use crate::analysis::diagnostics::lsp_severity;
use crate::analysis::project::Project;
use crate::analysis::source::DeclaredLanguage;
use crate::analysis::symbols::Namespace;
use crate::util::{byte_range_to_lsp_range, comline_diagnostic};

/// A use of a feature the declared language's generator refuses.
pub const UNSUPPORTED_FEATURE: &str = "generation::unsupported-feature";
/// A declaration no generator handles: an unknown language, a version that
/// isn't one, or one below the generator's floor.
pub const UNRESOLVED_TARGET: &str = "generation::unresolved-target";
/// A declaration below what a dependency declares for the same language.
pub const BELOW_DEPENDENCY: &str = "generation::below-dependency";
/// A declaration past what its generator has verified - the same code
/// `comline build` warns under, and settings can raise the same way.
pub const UNVERIFIED_VERSION: DiagnosticCode =
    DiagnosticCode::warning("generation::unverified-version");

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

/// The oldest version this build's generator for `language` handles - what
/// `comline new` scaffolds - or `None` when this build has none.
pub fn floor(language: &str) -> Option<&'static str> {
    registry()
        .targets()
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case(language))
        .map(|t| t.version)
}

/// The diagnostics for one schema's `units` (its namespace already among
/// them, or not - it isn't read), for every language in `languages`.
pub fn feature_diagnostics(
    source: &str,
    units: &[FrozenUnit],
    languages: &[DeclaredLanguage],
) -> Vec<Diagnostic> {
    // Only the schema's own features: the request options (`lib` mode,
    // `external_std`, ...) live in `comline.toml`, not here.
    let uses = features::used(&[(String::new(), units.to_vec())], &Options::default());
    let mut diagnostics = Vec::new();

    for declared in languages {
        let Ok(resolved) = resolve(&declared.language, &declared.version) else {
            continue;
        };
        let target = format!("{}#{}", declared.language, declared.version);
        for u in &uses {
            let Some(span) = u.span else { continue };
            let s = resolved.support(u.feature);
            let (severity, code, message) = match s.state {
                State::Unsupported => (
                    DiagnosticSeverity::WARNING,
                    UNSUPPORTED_FEATURE,
                    format!(
                        "{} can't be generated for `{target}`{} - `comline generation` refuses this schema for it",
                        u.feature.title(),
                        detail(&declared.language, &s)
                    ),
                ),
                State::Ignored => (
                    lsp_severity(checks::IGNORED_FEATURE.severity),
                    checks::IGNORED_FEATURE.id,
                    format!("`{target}` generates this without its {}{}", u.feature.title(), detail(&declared.language, &s)),
                ),
                State::Supported | State::Partial => continue,
            };
            diagnostics.push(comline_diagnostic(
                byte_range_to_lsp_range(source, span.0, span.1),
                severity,
                Some(code),
                message,
            ));
        }
    }
    diagnostics
}

/// The page the generator for `language#version` names for its connections
/// (`Topic::Connections`), if this build has the generator and it has one.
pub fn connections_doc(language: &str, version: &str) -> Option<&'static str> {
    let resolved = resolve(language, version).ok()?;
    docs::link(resolved.generator.docs, Topic::Connections)
}

/// What each framing the package can see states, by canonical name: the
/// encodings `configs` declare (the package's `config.idp` and its
/// dependencies'), as `comline generation` reads them for its checks.
pub fn framings<'a>(
    configs: impl IntoIterator<Item = &'a ConfigUnit>,
) -> BTreeMap<String, EncodingMapping> {
    configs
        .into_iter()
        .filter_map(|unit| match unit {
            ConfigUnit::Encoding {
                name,
                grammar,
                calls,
                ..
            } => Some((
                comline_codegen::encodings::canonical(name),
                EncodingMapping {
                    grammar: grammar.clone(),
                    calls: calls.clone(),
                    ..Default::default()
                },
            )),
            _ => None,
        })
        .collect()
}

/// The package's own schemas as `comline generation` hands them to its
/// checks: each by its namespace (`a/b`), frozen with the package's framing
/// and handshake in every protocol that doesn't write its own, in namespace
/// order so that "the first protocol" doesn't depend on which file the
/// editor listed first. `None` where the view isn't the whole package - no
/// manifest to say what its wire is, or a schema that doesn't parse, which
/// might hold the protocol that decides - and so can't be judged.
pub fn package_schemas(project: &Project) -> Option<Vec<(String, Vec<FrozenUnit>)>> {
    let config = project.config.as_deref()?;
    if !project.unparsed.is_empty() {
        return None;
    }

    let mut schemas: Vec<(String, Vec<FrozenUnit>)> = project
        .docs
        .iter()
        .filter(|d| d.dependency.is_none())
        .map(|d| {
            (
                d.namespace.join("/"),
                IncrementalInterpreter::from_declarations(d.document.0.clone()),
            )
        })
        .collect();
    schemas.sort_by(|a, b| a.0.cmp(&b.0));

    let mut units: Vec<&mut [FrozenUnit]> = schemas
        .iter_mut()
        .map(|(_, units)| units.as_mut_slice())
        .collect();
    apply_package_wire(&mut units, config);
    Some(schemas)
}

/// A generation request for `schemas` (as [`package_schemas`] has them) in
/// a package whose `wire.format` is `wire_format`, with `framings` and
/// `async_api` as one target would have them. It's what
/// `comline_codegen::checks` reads of a request: the target, the package's
/// name and version, and the options no check looks at are placeholders.
pub fn request(
    schemas: Vec<(String, Vec<FrozenUnit>)>,
    wire_format: &str,
    framings: BTreeMap<String, EncodingMapping>,
    async_api: bool,
) -> GenerateRequest {
    GenerateRequest {
        mode: Mode::Code,
        schemas,
        package: PackageMeta {
            name: String::new(),
            version: "0.0.0".to_string(),
        },
        target: TargetLanguage {
            language: String::new(),
            version: String::new(),
        },
        wire_format: wire_format.to_string(),
        external_std: false,
        validate_on_decode: Default::default(),
        async_api,
        encodings: framings,
        facts: None,
    }
}

/// What `comline generation` says about the package's connection that
/// doesn't depend on a target, at the protocol it's about in `docs[doc]`: a
/// warning where the protocols on a connection disagree on its framing or
/// handshake ([`checks::NO_PACKAGE_CONNECTION`], at the first that
/// disagrees), an error where two protocols one side provides have bare
/// method names that clash ([`checks::METHOD_NAME_CLASH`], at the second).
/// Each is in the schema that declares its protocol and nowhere else, so a
/// protocol in another file leaves this one without it, however many
/// languages the package declares.
pub fn connection_diagnostics(project: &Project, doc: usize) -> Vec<Diagnostic> {
    let here = &project.docs[doc];
    let Some(config) = project.config.as_deref() else {
        return vec![];
    };
    // Nothing to show in a file with no protocol, and nothing to work out for it.
    if !here
        .symbols
        .all_symbols()
        .iter()
        .any(|s| s.kind == SymbolKind::INTERFACE)
    {
        return vec![];
    }
    let Some(schemas) = package_schemas(project) else {
        return vec![];
    };

    let framings = framings(
        config.iter().chain(
            project
                .dependency_configs
                .iter()
                .flat_map(|(_, units)| units),
        ),
    );
    let request = request(
        schemas,
        config::wire_format(config),
        framings.clone(),
        false,
    );
    let mut found: Vec<CoreDiagnostic> = checks::generation(&request, None, None)
        .into_iter()
        .filter(|d| d.code == checks::NO_PACKAGE_CONNECTION)
        .collect();
    found.extend(checks::method_name_clashes(&request.schemas, &framings));

    // The check names a protocol by path (`app::Chat`); here it's the name in
    // this file's own namespace.
    let namespace = here.namespace.join("/");
    found
        .into_iter()
        .filter_map(|d| {
            let (schema, name) = d.unit.as_deref()?.rsplit_once("::")?;
            if schema != namespace {
                return None;
            }
            let symbol = here
                .symbols
                .get_in(name, Namespace::Declaration)
                .filter(|s| s.kind == SymbolKind::INTERFACE)?;
            Some(comline_diagnostic(
                byte_range_to_lsp_range(here.source, symbol.name_span.0, symbol.name_span.1),
                lsp_severity(d.code.severity),
                Some(d.code.id),
                d.message,
            ))
        })
        .collect()
}

/// `: note (tracking)`, or as much of it as there is; for a feature gated
/// above the declared version, the version to declare.
fn detail(language: &str, s: &Support) -> String {
    let mut out = String::new();
    if let Some(note) = s.note {
        out.push_str(&format!(": {note}"));
    }
    if let (State::Unsupported, Some(since)) = (s.state, s.since) {
        out.push_str(&format!(
            " - declare `{language}#{}` or later",
            since.version
        ));
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
            diagnostics.push(comline_diagnostic(
                byte_range_to_lsp_range(manifest, range.0, range.1),
                severity,
                Some(code),
                message,
            ))
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
                (None, Some(w)) => Some(lsp_severity(w.effective_severity())),
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
        by_state.entry(rank).or_default().push(format!(
            "- {}{}",
            feature.title(),
            detail(language, &s)
        ));
    }

    let versions = resolved.generator.versions;
    let mut out = format!(
        "**`{language}#{version}`** supports {supported} of {} features.",
        Feature::ALL.len()
    );
    out.push_str(&format!(
        "\n\nThe {language} generator handles {} or later, verified up to {}{}.",
        versions.floor,
        versions.verified_up_to,
        if versions.forward_compatible {
            "; newer toolchains build its output too"
        } else {
            ""
        }
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

    /// The generation diagnostics of the package's `files` (a manifest is
    /// added, declaring `languages`) for the one named `shown`.
    fn found_in(
        languages: &str,
        files: &[(&str, &str)],
        shown: &str,
    ) -> Vec<lsp_types::Diagnostic> {
        let manifest = format!(
            "congregation app\nspecification_version = 1\n\ncode_generation = {{\n    languages = {{\n{languages}    }}\n}}\n"
        );
        let mut all = vec![(Url::parse("file:///pkg/config.idp").unwrap(), manifest)];
        for (name, schema) in files {
            all.push((
                Url::parse(&format!("file:///pkg/src/{name}")).unwrap(),
                schema.to_string(),
            ));
        }
        let project = Project::new(all.iter());
        let doc = project
            .docs
            .iter()
            .position(|d| d.uri.path().ends_with(shown))
            .unwrap();
        project_diagnostics(&project, doc)
            .into_iter()
            .filter(|d| {
                matches!(&d.code, Some(lsp_types::NumberOrString::String(code))
                    if code.starts_with("generation::"))
            })
            .collect()
    }

    /// The package's diagnostics for `schema`, with `languages` declared.
    fn diagnostics(languages: &str, schema: &str) -> Vec<(String, String, u32, u32)> {
        found_in(languages, &[("api.ids", schema)], "api.ids")
            .into_iter()
            .filter_map(|d| match d.code {
                Some(lsp_types::NumberOrString::String(code)) => {
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
        assert!(
            message.starts_with("unions can't be generated for `rust#1.85.0`: "),
            "{message}"
        );
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
    fn a_feature_a_declared_language_ignores_is_a_warning_at_each_use() {
        let schema = "validator NonEmpty for string {\n    validate {\n        \
                      assert(value.length > 0, f\"{value.name} is empty\")\n    }\n}\n\n\
                      @validators = [NonEmpty]\ntype Name = string\n\n\
                      @validators = [NonEmpty]\ntype Title = string\n";
        let found = found_in(
            "        typescript#5.0 = {}\n        rust#1.85.0 = {}\n",
            &[("api.ids", schema)],
            "api.ids",
        );
        assert_eq!(found.len(), 2, "{found:?}");
        for d in &found {
            assert_eq!(
                d.code,
                Some(lsp_types::NumberOrString::String(
                    comline_codegen::checks::IGNORED_FEATURE.id.to_string()
                ))
            );
            assert_eq!(d.severity, Some(lsp_types::DiagnosticSeverity::WARNING));
            assert_eq!(d.source.as_deref(), Some("comline"));
            assert!(
                d.message
                    .starts_with("`typescript#5.0` generates this without its validators: "),
                "{}",
                d.message
            );
        }
        let at: Vec<_> = found
            .iter()
            .map(|d| (d.range.start.line, d.range.start.character))
            .collect();
        assert_eq!(at, [(6, 15), (9, 15)], "at each validator's name");
    }

    /// The `(code, severity, line, text at the range)` of each diagnostic.
    fn at(found: &[lsp_types::Diagnostic], source: &str) -> Vec<(String, String, u32, String)> {
        found
            .iter()
            .map(|d| {
                let code = match &d.code {
                    Some(lsp_types::NumberOrString::String(code)) => code.clone(),
                    _ => String::new(),
                };
                let line = source.lines().nth(d.range.start.line as usize).unwrap();
                let text = line[d.range.start.character as usize..d.range.end.character as usize]
                    .to_string();
                (
                    code,
                    format!("{:?}", d.severity.unwrap()),
                    d.range.start.line,
                    text,
                )
            })
            .collect()
    }

    /// Two protocols the client provides on JSON-RPC, each with a `send`.
    const CLASHING: &str = "@provider = Client\n@framing = \"jsonrpc\"\nprotocol Chat {\n    \
                            function send();\n}\n\n@provider = Client\n@framing = \"jsonrpc\"\n\
                            protocol Admin {\n    function send();\n}\n";

    #[test]
    fn bare_method_names_that_clash_are_an_error_at_the_second_protocol() {
        let found = found_in("", &[("api.ids", CLASHING)], "api.ids");
        assert_eq!(
            at(&found, CLASHING),
            [(
                "generation::method-name-clash".to_string(),
                "Error".to_string(),
                8,
                "Admin".to_string()
            )]
        );
        assert_eq!(found[0].source.as_deref(), Some("comline"));
        assert!(
            found[0]
                .message
                .starts_with("protocols `api::Chat` and `api::Admin` both have a function `send`"),
            "{}",
            found[0].message
        );
        // The same, whatever languages are declared.
        assert_eq!(diagnostics("        rust#1.85.0 = {}\n", CLASHING).len(), 1);
    }

    #[test]
    fn a_clash_is_shown_once_in_the_file_of_the_second_protocol() {
        let (first, second) = CLASHING.split_once("\n\n").unwrap();
        let files = [("a.ids", first), ("b.ids", second)];
        assert_eq!(found_in("", &files, "a.ids"), []);
        let found = found_in("", &files, "b.ids");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .message
                .starts_with("protocols `a::Chat` and `b::Admin` "),
            "{}",
            found[0].message
        );
        // The order is the namespaces', not the order the files were listed in.
        let files = [("b.ids", second), ("a.ids", first)];
        assert_eq!(found_in("", &files, "a.ids"), []);
        assert_eq!(found_in("", &files, "b.ids").len(), 1);
    }

    /// Two protocols on connections that disagree on the handshake.
    const DISAGREEING: &str = "protocol Chat {\n    function send();\n}\n\n\
                               @handshake = \"none\"\nprotocol Admin {\n    function kick();\n}\n";

    #[test]
    fn protocols_on_connections_that_disagree_are_a_warning_at_the_one_that_does() {
        let found = found_in("", &[("api.ids", DISAGREEING)], "api.ids");
        assert_eq!(
            at(&found, DISAGREEING),
            [(
                "generation::no-package-connection".to_string(),
                "Warning".to_string(),
                5,
                "Admin".to_string()
            )]
        );
        assert!(
            found[0]
                .message
                .starts_with("no package `serve` / `connect` - `api::Chat` runs the"),
            "{}",
            found[0].message
        );
        assert_eq!(
            diagnostics(
                "        rust#1.85.0 = {}\n        typescript#5.0 = {}\n",
                DISAGREEING
            )
            .len(),
            1,
            "once, not per language"
        );
    }

    #[test]
    fn protocols_that_agree_say_nothing_about_the_connection() {
        let agreeing = "protocol Chat {\n    function send();\n}\n\nprotocol Admin {\n    function send();\n}\n";
        assert_eq!(diagnostics("", agreeing), []);
        assert_eq!(diagnostics("", "struct User {\n    name: string\n}\n"), []);
    }

    #[test]
    fn the_summary_lists_what_isnt_fully_supported() {
        let summary = super::summary("rust", "1.85.0").unwrap();
        assert!(
            summary.starts_with("**`rust#1.85.0`** supports "),
            "{summary}"
        );
        assert!(
            summary.contains("Not supported (generation refuses a schema using these):\n- "),
            "{summary}"
        );
        assert!(summary.contains("\n- unions: "), "{summary}");
        assert_eq!(super::summary("python", "3.11.0"), None);
    }

    #[test]
    fn the_summary_names_the_generators_range_or_why_it_has_none() {
        let summary = super::summary("rust", "1.88.0").unwrap();
        assert!(
            summary.contains("The rust generator handles 1.85.0 or later, verified up to 1.99"),
            "{summary}"
        );
        let below = super::summary("rust", "1.70.0").unwrap();
        assert!(below.contains("it needs `rust#1.85.0` or later"), "{below}");
    }

    /// `declaration_diagnostics` for `languages`, with `rest` after the
    /// languages block and `dependencies` declaring.
    fn declarations(
        languages: &str,
        rest: &str,
        dependencies: &[(&str, &str)],
    ) -> Vec<(String, String, String)> {
        let manifest = format!(
            "congregation app\nspecification_version = 1\n\ncode_generation = {{\n    languages = {{\n{languages}    }}\n}}\n{rest}"
        );
        let languages = crate::analysis::source::declared_languages(&manifest);
        let settings = crate::analysis::source::declared_settings(&manifest);
        let dependencies: Vec<_> = dependencies
            .iter()
            .map(|(name, target)| {
                let (language, version) = target.split_once('#').unwrap();
                (
                    name.to_string(),
                    crate::analysis::source::DeclaredLanguage {
                        language: language.into(),
                        version: version.into(),
                    },
                )
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
                    .map(|l| {
                        l[d.range.start.character as usize..d.range.end.character as usize]
                            .to_string()
                    })
                    .unwrap_or_default();
                (
                    code,
                    format!("{:?}", d.severity.unwrap()),
                    format!("{at}: {}", d.message),
                )
            })
            .collect()
    }

    #[test]
    fn a_declaration_comline_build_refuses_is_an_error_at_the_entry() {
        let found = declarations(
            "        rust#1.70.0 = {}\n        python#3.11 = {}\n",
            "",
            &[],
        );
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, super::UNRESOLVED_TARGET);
        assert_eq!(found[0].1, "Error");
        assert!(found[0].2.starts_with("rust#1.70.0: "), "{found:?}");
        assert!(
            found[0].2.contains("it needs `rust#1.85.0` or later"),
            "{found:?}"
        );
        assert!(found[1].2.contains("has no python generator"), "{found:?}");
        assert_eq!(
            declarations(
                "        rust#1.85.0 = {}\n        typescript#5.4 = {}\n",
                "",
                &[]
            ),
            []
        );
    }

    #[test]
    fn a_declaration_below_a_dependencys_is_an_error() {
        let found = declarations(
            "        rust#1.85.0 = {}\n",
            "",
            &[("shared", "rust#1.90.0"), ("other", "rust#1.88.0")],
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, super::BELOW_DEPENDENCY);
        assert!(
            found[0]
                .2
                .contains("dependency `shared` declares `rust#1.90.0`"),
            "{found:?}"
        );
        assert_eq!(
            declarations(
                "        rust#1.90.0 = {}\n",
                "",
                &[("shared", "rust#1.90.0")]
            ),
            []
        );
    }

    #[test]
    fn past_the_verified_range_is_a_warning_settings_can_raise() {
        let found = declarations("        typescript#6.0 = {}\n", "", &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, super::UNVERIFIED_VERSION.id);
        assert_eq!(found[0].1, "Warning");
        assert!(
            found[0]
                .2
                .contains("past what the typescript generator has verified (up to 5.9)"),
            "{found:?}"
        );

        let raised = "settings = {\n    diagnostics = {\n        generation::unverified-version = {\n            level = \"error\"\n        }\n    }\n}\n";
        assert_eq!(
            declarations("        typescript#6.0 = {}\n", raised, &[])[0].1,
            "Error"
        );
        let allowed = raised.replace("\"error\"", "\"allow\"");
        assert_eq!(
            declarations("        typescript#6.0 = {}\n", &allowed, &[]),
            []
        );
        // Rust promises newer compilers build older code: verified.
        assert_eq!(declarations("        rust#2.5 = {}\n", "", &[]), []);
    }
}
