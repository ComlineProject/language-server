//! What the analysis layer is told about each file of a project view.
//!
//! Most files say everything with their URI and text: a schema's namespace
//! follows from its path under `src/`. A dependency's files can't: to its own
//! package, `shared-types/src/models.ids` is `models`; to a package that
//! declares it as `shared_types`, it's `shared_types::models`. So a file can
//! also carry the namespace it's seen under, and the dependency it comes from,
//! which makes it read-only to the package being edited.
//!
//! The package's own manifest, `config.idp`, can be part of the view too:
//! that's where the analysis learns which dependencies are declared, so a
//! `use` of one it has no files for isn't mistaken for a typo.

use comline_core::package::config::dependency::{DependencyConfig, DependencySource};
use lsp_types::Url;

use crate::parser;

/// One file of a project view.
pub trait ProjectSource {
    fn uri(&self) -> &Url;
    fn text(&self) -> &str;
    /// The namespace this file is seen under. `None`: derive it from the URI.
    fn namespace(&self) -> Option<&[String]> {
        None
    }
    /// The dependency this file belongs to, `None` for the package's own.
    fn dependency(&self) -> Option<&str> {
        None
    }
}

impl ProjectSource for (Url, String) {
    fn uri(&self) -> &Url {
        &self.0
    }
    fn text(&self) -> &str {
        &self.1
    }
}

/// A [`ProjectSource`] that can carry all of it - what the server hands over.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub uri: Url,
    pub text: String,
    pub namespace: Option<Vec<String>>,
    pub dependency: Option<String>,
}

impl SourceFile {
    /// A file of the package itself (or its `config.idp`).
    pub fn local(uri: Url, text: String) -> Self {
        Self { uri, text, namespace: None, dependency: None }
    }

    /// A file of the dependency declared as `name`, seen under `namespace`.
    pub fn of_dependency(uri: Url, text: String, name: &str, namespace: Vec<String>) -> Self {
        Self { uri, text, namespace: Some(namespace), dependency: Some(name.to_string()) }
    }
}

impl ProjectSource for SourceFile {
    fn uri(&self) -> &Url {
        &self.uri
    }
    fn text(&self) -> &str {
        &self.text
    }
    fn namespace(&self) -> Option<&[String]> {
        self.namespace.as_deref()
    }
    fn dependency(&self) -> Option<&str> {
        self.dependency.as_deref()
    }
}

/// Whether `uri` is a package manifest (`config.idp`) rather than a schema.
pub fn is_manifest(uri: &Url) -> bool {
    uri.path().ends_with(".idp")
}

/// A dependency declared in the package's `config.idp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredDependency {
    pub name: String,
    pub kind: DependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyKind {
    /// `path = "..."`, as written.
    Path(String),
    Git,
    Registry,
}

/// Every dependency `config.idp`'s text declares, by name - read with core's
/// own parser, so the editor and `comline check` agree on what's declared.
/// Empty when the manifest doesn't parse (mid-edit) or declares none.
pub fn declared_dependencies(manifest: &str) -> Vec<DeclaredDependency> {
    let Some(congregation) = parser::parse_idp(manifest).ok().and_then(|r| r.document) else {
        return vec![];
    };
    let Ok(dependencies) = DependencyConfig::parse_dependencies(&congregation.assignments) else {
        return vec![];
    };

    let mut declared: Vec<DeclaredDependency> = dependencies
        .into_values()
        .map(|dep| DeclaredDependency {
            kind: match &dep.source {
                DependencySource::Path { path, .. } => DependencyKind::Path(path.to_string_lossy().into_owned()),
                DependencySource::Git { .. } => DependencyKind::Git,
                DependencySource::Registry { .. } => DependencyKind::Registry,
            },
            name: dep.name,
        })
        .collect();
    declared.sort_by(|a, b| a.name.cmp(&b.name));
    declared
}

/// The package-level `settings` dict `config.idp`'s text declares - read
/// with core's own interpreter, so the editor and `comline build` agree
/// on what's forbidden. Empty when the manifest doesn't parse (mid-edit),
/// declares no `settings` key, or fails to interpret for any reason -
/// never panics, never blocks the editor.
///
/// Doesn't resolve a cross-package reference (`settings = dep::settings::
/// Name`) - core's own interpreter has nothing to resolve it against with
/// no override supplied, so this reads back an empty dict for that case
/// instead of calling into core at all: core's freezing code *panics* on
/// a `settings` value it can't make sense of with no override available
/// (by design - see its own doc comment), so this has to recognize and
/// skip a reference up front rather than let that panic happen. See
/// [`resolve_cross_package_settings`] for resolving one for real.
pub fn declared_settings(manifest: &str) -> comline_core::settings::SettingsDict {
    use comline_core::package::config::idl::grammar::{Key, Value};
    use comline_core::package::config::ir::context::ProjectContext;
    use comline_core::package::config::ir::frozen;
    use comline_core::package::config::ir::interpreter::interpret::interpret_context;

    let Some(congregation) = parser::parse_idp(manifest).ok().and_then(|r| r.document) else {
        return Default::default();
    };

    let is_cross_package_reference = congregation.assignments.iter().any(|a| {
        let a = &a.value;
        matches!(&a.key, Key::Identifier(id) if id.value == "settings")
            && matches!(&a.value, Value::Namespaced(_))
    });
    if is_cross_package_reference {
        return Default::default();
    }

    let context = ProjectContext::with_config(congregation);
    let Ok(units) = interpret_context(&context, None) else {
        return Default::default();
    };
    frozen::settings(&units).cloned().unwrap_or_default()
}

/// `settings = <dependency>::settings::<name>` resolved live, against
/// whatever dependency schema files are already in `other_files` - no new
/// fetch, no disk I/O, no full `core::package::build` compile. Mirrors
/// `core`'s own `interpret::resolve_cross_package_settings` algorithm
/// (same shape requirement, same ambiguity rule), just reading from
/// already-in-hand [`SourceFile`]s (already fetched/cached by
/// `workspace`/`dependencies`) instead of a fresh `package::deps::resolve`.
///
/// `None`: the manifest's `settings` (if present at all) isn't a
/// cross-package reference - nothing to resolve here, [`declared_settings`]
/// already handles every other shape. `Some(Err(_))`: it IS a reference,
/// but resolution failed - a caller turns this into a diagnostic, not a
/// panic; never blocks the editor on malformed/mid-edit text.
pub fn resolve_cross_package_settings<S: ProjectSource>(
    manifest: &str,
    other_files: &[S],
) -> Option<Result<comline_core::settings::SettingsDict, String>> {
    use comline_core::package::config::idl::grammar::{Key, Value};

    let congregation = parser::parse_idp(manifest).ok().and_then(|r| r.document)?;
    let assignment = congregation.assignments.iter().find_map(|a| {
        let a = &a.value;
        matches!(&a.key, Key::Identifier(id) if id.value == "settings").then_some(a)
    })?;
    let Value::Namespaced(ns) = &assignment.value else {
        return None;
    };

    let segments: Vec<&str> = ns.value.split("::").collect();
    let [dep_name, "settings", block_name] = segments.as_slice() else {
        return Some(Err(format!(
            "settings = {}: a cross-package reference must have the shape \
             <dependency>::settings::<name>",
            ns.value
        )));
    };

    Some(
        resolve_named_settings_block(manifest, dep_name, block_name, other_files)
            .map_err(|e| format!("settings = {}: {e}", ns.value)),
    )
}

/// Every schema file belonging to the `dep_name` dependency in
/// `other_files`, scanned for a named `settings <block_name> { ... }`
/// block. More than one match across different files is ambiguous - a
/// hard error naming both, same philosophy as core's own duplicate-key
/// handling: don't silently pick one.
fn resolve_named_settings_block<S: ProjectSource>(
    manifest: &str,
    dep_name: &str,
    block_name: &str,
    other_files: &[S],
) -> Result<comline_core::settings::SettingsDict, String> {
    use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
    use comline_core::schema::ir::compiler::Compile;
    use comline_core::schema::ir::frozen::unit::FrozenUnit as SchemaFrozenUnit;

    if !declared_dependencies(manifest).iter().any(|d| d.name == dep_name) {
        return Err(format!("no dependency named `{dep_name}` is declared"));
    }

    let mut found: Option<(comline_core::settings::SettingsDict, String)> = None;
    for file in other_files {
        if file.dependency() != Some(dep_name) || !file.uri().path().ends_with(".ids") {
            continue;
        }
        let Some(document) = parser::parse(file.text()).ok().and_then(|r| r.document) else {
            continue;
        };
        let units = IncrementalInterpreter::from_declarations(document.0);
        for unit in &units {
            let SchemaFrozenUnit::Settings { name: Some(n), values, .. } = unit else { continue };
            if n != block_name {
                continue;
            }
            let here =
                file.namespace().map(|ns| ns.join("::")).unwrap_or_else(|| file.uri().to_string());
            if let Some((_, first)) = &found {
                return Err(format!(
                    "settings block `{block_name}` is ambiguous: found in both `{first}` and `{here}`"
                ));
            }
            found = Some((values.clone(), here));
        }
    }

    found
        .map(|(dict, _)| dict)
        .ok_or_else(|| format!("package `{dep_name}` has no settings block named `{block_name}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_kind_of_declared_dependency() {
        let manifest = "congregation app\nspecification_version = 1\n\n\
            dependencies = {\n    \
                shared = {\n        path = \"../shared\"\n    }\n    \
                net = {\n        version = \"1.0.0\"\n        uri = \"https://example.test/net\"\n        commit = \"abc\"\n    }\n    \
                hosted = {\n        version = \"1.0.0\"\n        uri = \"comline://example.test/hosted\"\n    }\n\
            }\n";

        assert_eq!(
            declared_dependencies(manifest),
            vec![
                DeclaredDependency { name: "hosted".into(), kind: DependencyKind::Registry },
                DeclaredDependency { name: "net".into(), kind: DependencyKind::Git },
                DeclaredDependency { name: "shared".into(), kind: DependencyKind::Path("../shared".into()) },
            ]
        );
    }

    #[test]
    fn a_manifest_mid_edit_declares_nothing() {
        assert!(declared_dependencies("congregation app\ndependencies = {").is_empty());
        assert!(declared_dependencies("congregation app\nspecification_version = 1\n").is_empty());
    }

    #[test]
    fn declared_settings_reads_a_well_formed_dict() {
        use comline_core::settings::value::SettingsValue;

        let manifest = "congregation app\nspecification_version = 1\n\
            settings = {\n    validators = {\n        allowed = false\n    }\n}\n";

        let settings = declared_settings(manifest);
        let SettingsValue::Dict(validators) = settings.get("validators").unwrap() else {
            panic!("expected a dict at 'validators'");
        };
        assert_eq!(validators.get("allowed"), Some(&SettingsValue::Bool(false)));
    }

    #[test]
    fn declared_settings_is_empty_with_no_settings_key() {
        let manifest = "congregation app\nspecification_version = 1\n";
        assert!(declared_settings(manifest).is_empty());
    }

    #[test]
    fn declared_settings_is_empty_mid_edit() {
        assert!(declared_settings("congregation app\nsettings = {").is_empty());
    }

    #[test]
    fn declared_settings_does_not_panic_on_a_cross_package_reference() {
        // Core's own freezing code *panics* on a `settings` value it can't
        // resolve with no override available (by design, for every other
        // malformed shape too) - this must recognize and skip a reference
        // before ever reaching that path, not rely on catching a panic.
        let manifest = "congregation app\nspecification_version = 1\n\
            settings = shared::settings::Strict\n";
        assert!(declared_settings(manifest).is_empty());
    }

    fn consumer_manifest() -> String {
        "congregation app\nspecification_version = 1\n\n\
         dependencies = {\n    shared = {\n        path = \"../shared\"\n    }\n}\n\n\
         settings = shared::settings::Strict\n"
            .to_string()
    }

    fn dep_file(namespace: &[&str], text: &str) -> SourceFile {
        SourceFile::of_dependency(
            Url::parse("file:///shared/src/policy.ids").unwrap(),
            text.to_string(),
            "shared",
            namespace.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn a_non_reference_settings_value_resolves_to_none() {
        let manifest = "congregation app\nspecification_version = 1\n\
            settings = {\n    allowed = true\n}\n";
        assert!(resolve_cross_package_settings::<SourceFile>(manifest, &[]).is_none());
    }

    #[test]
    fn no_settings_key_at_all_resolves_to_none() {
        let manifest = "congregation app\nspecification_version = 1\n";
        assert!(resolve_cross_package_settings::<SourceFile>(manifest, &[]).is_none());
    }

    #[test]
    fn a_cross_package_reference_resolves_against_the_dependencys_files() {
        use comline_core::settings::value::SettingsValue;

        let files = [dep_file(&["shared", "policy"], "settings Strict {\n    max_depth = 4\n}\n")];
        let result = resolve_cross_package_settings(&consumer_manifest(), &files)
            .expect("a reference should resolve to Some")
            .expect("resolution should succeed");
        assert_eq!(result.get("max_depth"), Some(&SettingsValue::Integer(4)));
    }

    #[test]
    fn a_reference_to_an_undeclared_dependency_is_an_error() {
        let manifest = "congregation app\nspecification_version = 1\n\
            settings = nonexistent::settings::Strict\n";
        let err = resolve_cross_package_settings::<SourceFile>(manifest, &[])
            .expect("a reference should resolve to Some")
            .expect_err("no such dependency is declared");
        assert!(err.contains("no dependency named `nonexistent`"), "got: {err}");
    }

    #[test]
    fn a_reference_to_a_missing_block_is_an_error() {
        let files = [dep_file(&["shared", "policy"], "settings Other {\n    max_depth = 4\n}\n")];
        let err = resolve_cross_package_settings(&consumer_manifest(), &files)
            .expect("a reference should resolve to Some")
            .expect_err("shared has no block named Strict");
        assert!(err.contains("no settings block named `Strict`"), "got: {err}");
    }

    #[test]
    fn an_ambiguous_block_across_two_files_is_an_error() {
        let files = [
            dep_file(&["shared", "a"], "settings Strict {\n    max_depth = 4\n}\n"),
            dep_file(&["shared", "b"], "settings Strict {\n    max_depth = 8\n}\n"),
        ];
        let err = resolve_cross_package_settings(&consumer_manifest(), &files)
            .expect("a reference should resolve to Some")
            .expect_err("two files both define Strict - ambiguous");
        assert!(err.contains("ambiguous"), "got: {err}");
    }

    #[test]
    fn malformed_shape_without_exactly_settings_in_the_middle_is_an_error() {
        let manifest = "congregation app\nspecification_version = 1\n\
            settings = shared::other::Strict\n";
        let err = resolve_cross_package_settings::<SourceFile>(manifest, &[])
            .expect("a namespaced value should resolve to Some")
            .expect_err("the middle segment must be literally 'settings'");
        assert!(err.contains("<dependency>::settings::<name>"), "got: {err}");
    }
}
