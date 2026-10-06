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
pub fn declared_settings(manifest: &str) -> comline_core::settings::SettingsDict {
    use comline_core::package::config::ir::context::ProjectContext;
    use comline_core::package::config::ir::frozen;
    use comline_core::package::config::ir::interpreter::interpret::interpret_context;

    let Some(congregation) = parser::parse_idp(manifest).ok().and_then(|r| r.document) else {
        return Default::default();
    };
    let context = ProjectContext::with_config(congregation);
    let Ok(units) = interpret_context(&context) else {
        return Default::default();
    };
    frozen::settings(&units).cloned().unwrap_or_default()
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
}
