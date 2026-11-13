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
        Self {
            uri,
            text,
            namespace: None,
            dependency: None,
        }
    }

    /// A file of the dependency declared as `name`, seen under `namespace`.
    pub fn of_dependency(uri: Url, text: String, name: &str, namespace: Vec<String>) -> Self {
        Self {
            uri,
            text,
            namespace: Some(namespace),
            dependency: Some(name.to_string()),
        }
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

/// The package's own `config.idp` among `files`, not a dependency's.
pub fn own_manifest<S: ProjectSource>(files: &[S]) -> Option<&S> {
    files
        .iter()
        .find(|file| is_manifest(file.uri()) && file.dependency().is_none())
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
    let Some(congregation) = parser::parse_congregation(manifest) else {
        return vec![];
    };
    let Ok(dependencies) = DependencyConfig::parse_dependencies(&congregation.assignments) else {
        return vec![];
    };

    let mut declared: Vec<DeclaredDependency> = dependencies
        .into_values()
        .map(|dep| DeclaredDependency {
            kind: match &dep.source {
                DependencySource::Path { path, .. } => {
                    DependencyKind::Path(path.to_string_lossy().into_owned())
                }
                DependencySource::Git { .. } => DependencyKind::Git,
                DependencySource::Registry { .. } => DependencyKind::Registry,
            },
            name: dep.name,
        })
        .collect();
    declared.sort_by(|a, b| a.name.cmp(&b.name));
    declared
}

/// A language `config.idp`'s `code_generation.languages` declares:
/// `rust#1.85.0` -> `rust`, `1.85.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredLanguage {
    pub language: String,
    pub version: String,
}

/// The edition `config.idp` names (core#102), as core reads it: the newest
/// when the manifest names none it knows or doesn't parse - `config.idp`'s
/// own diagnostics say what's wrong with it.
pub fn declared_edition(manifest: &str) -> comline_core::edition::Edition {
    use comline_core::package::config::ir::context::ProjectContext;
    parser::parse_congregation(manifest)
        .map(|congregation| ProjectContext::with_config(congregation).edition())
        .unwrap_or(comline_core::edition::Edition::LATEST)
}

/// Every language `config.idp`'s text declares, read with core's own
/// interpreter, as `comline generation` reads them. Empty when the manifest
/// doesn't parse (mid-edit) or doesn't interpret.
pub fn declared_languages(manifest: &str) -> Vec<DeclaredLanguage> {
    use comline_core::package::config::ir::frozen::FrozenUnit;

    // `settings` has nothing to do with the languages, and a cross-package
    // reference there can't be interpreted on its own (see
    // `declared_settings`); `declared_config` leaves it out.
    let Some(units) = declared_config(manifest) else {
        return vec![];
    };
    units
        .iter()
        .filter_map(|u| match u {
            FrozenUnit::CodeGeneration(details) => {
                details
                    .name
                    .split_once('#')
                    .map(|(language, version)| DeclaredLanguage {
                        language: language.to_string(),
                        version: version.to_string(),
                    })
            }
            _ => None,
        })
        .collect()
}

/// The settings every package has underneath its own, whether or not a
/// manifest is in view: Comline's built-in defaults.
///
/// The editor used to fall back to an *empty* dict when it could not see a
/// manifest, which made it disagree with every build. `@framing` is the
/// visible case - it is forbidden by the built-in default, so hover
/// called it "allowed by default" and completion offered it without
/// comment, and accepting that suggestion produced a hard build error.
pub fn base_settings() -> comline_core::settings::SettingsDict {
    comline_core::settings::defaults::built_in()
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

    // Every fallback here is the built-in base, not an empty dict. A
    // manifest that is mid-edit, unreadable, or outright invalid tells us
    // nothing about the package's settings - but it does not remove the
    // floor every package has underneath its own. Falling back to empty
    // made the editor claim `@framing` was allowed by default in exactly
    // the situations where it could not tell.
    let Some(mut congregation) = parser::parse_congregation(manifest) else {
        return base_settings();
    };
    without_encodings(&mut congregation);

    let is_cross_package_reference = congregation.assignments.iter().any(|a| {
        let a = &a.value;
        matches!(&a.key, Key::Identifier(id) if id.value == "settings")
            && matches!(&a.value, Value::Namespaced(_))
    });
    if is_cross_package_reference {
        return base_settings();
    }

    let context = ProjectContext::with_config(congregation);
    let Ok(units) = interpret_context(&context, None) else {
        return base_settings();
    };
    frozen::settings(&units).cloned().unwrap_or_default()
}

/// Why this manifest could not be interpreted, if it could not be.
///
/// `.idp` interpretation used to `panic!` on every one of these, which
/// killed the server outright (core#88) — so there was never an error here
/// to report, and a manifest that a build rejects produced no editor
/// feedback at all. It returns an error now, so the manifest's own
/// diagnostics can say so.
///
/// `None` for a manifest that is merely incomplete: a parse failure is
/// already reported by the parse pass, and reporting it twice is worse
/// than once.
pub fn manifest_error(manifest: &str, path: Option<&std::path::Path>) -> Option<String> {
    use comline_core::package::config::ir::context::{Origin, ProjectContext};
    use comline_core::package::config::ir::interpreter::interpret::interpret_context;

    let mut congregation = parser::parse_congregation(manifest)?;
    let context = match path {
        // On disk, `encodings` is checked as a build checks it: each
        // profile read from beside the manifest.
        Some(path) => {
            ProjectContext::with_config_from_origin(Origin::Disk(path.to_path_buf()), congregation)
        }
        // Elsewhere there is no directory to read a profile from, so the
        // entries are left to the build rather than reported as unreadable.
        None => {
            without_encodings(&mut congregation);
            ProjectContext::with_config(congregation)
        }
    };
    interpret_context(&context, None)
        .err()
        .map(|e| e.to_string())
}

/// Where in `manifest` a [`manifest_error`] belongs: the top-level entry it's
/// about when that's `handshake` (its name or parameters), else the start
/// of the file.
pub fn manifest_error_span(manifest: &str, message: &str) -> (usize, usize) {
    let about = "handshake";
    if !message.contains(about) {
        return (0, 0);
    }
    parser::parse_congregation(manifest)
        .and_then(|congregation| {
            congregation
                .assignments
                .iter()
                .find(|a| a.value.key.text() == about)
                .map(|a| a.span)
        })
        .unwrap_or((0, 0))
}

/// What a manifest freezes to, for the wire and encodings the guarantee check
/// reads: core's own interpretation of `config.idp`, as `comline build` makes
/// it.
pub struct FrozenConfig {
    /// The frozen units, or `None` when the manifest can't be interpreted (it
    /// doesn't parse, or the build would refuse it): then nothing is known of
    /// the package's wire or encodings.
    pub units: Option<Vec<comline_core::package::config::ir::frozen::FrozenUnit>>,
    /// Whether `units` holds every encoding the manifest declares. An
    /// encoding freezes with the facts its profile states and whether vectors
    /// sit beside it, which only the package's directory can say: where the
    /// manifest isn't on disk, or a profile can't be read, the entries are
    /// left out and this is `false`.
    pub complete: bool,
}

/// `manifest` interpreted by core, its `encodings` read from the profiles
/// beside it when `path` (where the manifest lives on disk, if it does) allows.
///
/// `settings` is left out: it can be a cross-package reference, which has
/// nothing to resolve against here and which the guarantee check doesn't read
/// (the package's settings come from [`declared_settings`] and
/// [`resolve_cross_package_settings`]).
pub fn frozen_config(manifest: &str, path: Option<&std::path::Path>) -> FrozenConfig {
    use comline_core::package::config::ir::context::{Origin, ProjectContext};
    use comline_core::package::config::ir::interpreter::interpret::interpret_context;

    let Some(mut congregation) = parser::parse_congregation(manifest) else {
        return FrozenConfig {
            units: None,
            complete: false,
        };
    };
    congregation
        .assignments
        .retain(|a| a.value.key.text() != "settings");
    let declares_encodings = congregation
        .assignments
        .iter()
        .any(|a| a.value.key.text() == "encodings");

    if let Some(path) = path.filter(|_| declares_encodings) {
        let context = ProjectContext::with_config_from_origin(
            Origin::Disk(path.to_path_buf()),
            congregation.clone(),
        );
        if let Ok(units) = interpret_context(&context, None) {
            return FrozenConfig {
                units: Some(units),
                complete: true,
            };
        }
    }
    // Without the profiles: the rest of the manifest is still what it says.
    without_encodings(&mut congregation);
    match interpret_context(&ProjectContext::with_config(congregation), None) {
        Ok(units) => FrozenConfig {
            units: Some(units),
            complete: !declares_encodings,
        },
        Err(_) => FrozenConfig {
            units: None,
            complete: false,
        },
    }
}

/// Where `uri` is on disk, if it is. [`Url::to_file_path`] doesn't exist on
/// `wasm32-unknown-unknown` (see `analysis::imports`), where nothing is.
#[cfg(any(unix, windows, target_os = "redox", target_os = "wasi"))]
pub fn disk_path(uri: &Url) -> Option<std::path::PathBuf> {
    uri.to_file_path().ok()
}

#[cfg(not(any(unix, windows, target_os = "redox", target_os = "wasi")))]
pub fn disk_path(_: &Url) -> Option<std::path::PathBuf> {
    None
}

/// `congregation` without its `encodings` key. Core freezes an encoding by
/// reading its profile from the package directory, which the text alone
/// doesn't have, so the key is left out wherever only other keys matter.
/// The names it declares are read by [`declared_encodings`].
fn without_encodings(congregation: &mut comline_core::package::config::idl::grammar::Congregation) {
    congregation
        .assignments
        .retain(|a| a.value.key.text() != "encodings");
}

/// Every encoding `config.idp`'s text declares under `encodings`, by name
/// and kind (`"com.acme.x@1" = { kind = "framing", ... }`). Read from the
/// text alone, with no profile read, so it works on any file the editor
/// has: an entry whose `kind` isn't one of the three is left out, and the
/// build reports it. Empty when the manifest doesn't parse (mid-edit).
pub fn declared_encodings(manifest: &str) -> Vec<(String, comline_core::encodings::EncodingKind)> {
    declared_encoding_details(manifest)
        .into_iter()
        .map(|e| (e.name, e.kind))
        .collect()
}

/// An encoding `config.idp` declares, with the mappings it suggests per
/// language (`map = { rust = { type = "...", package = "..." } }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredEncoding {
    pub name: String,
    pub kind: comline_core::encodings::EncodingKind,
    /// By language id. An entry whose mapping is malformed is left out, and
    /// the build reports it.
    pub map: std::collections::BTreeMap<String, comline_core::encodings::EncodingMapping>,
}

/// [`declared_encodings`], with each encoding's suggested mappings.
pub fn declared_encoding_details(manifest: &str) -> Vec<DeclaredEncoding> {
    use comline_core::encodings::{EncodingKind, EncodingMapping};
    use comline_core::package::config::idl::grammar::Value;

    let Some(congregation) = parser::parse_congregation(manifest) else {
        return vec![];
    };
    let text = |value: &Value| match value {
        Value::String(s) => Some(s.value.trim_matches('"').to_string()),
        _ => None,
    };
    let mut declared = vec![];
    for assignment in &congregation.assignments {
        let assignment = &assignment.value;
        let (true, Value::Dictionary(entries)) =
            (assignment.key.text() == "encodings", &assignment.value)
        else {
            continue;
        };
        for entry in &entries.assignments {
            let Value::Dictionary(fields) = &entry.value.value else {
                continue;
            };
            let mut kind = None;
            let mut map = std::collections::BTreeMap::new();
            for field in &fields.assignments {
                match (field.value.key.text().as_str(), &field.value.value) {
                    ("kind", value) => {
                        kind = match text(value).as_deref() {
                            Some("format") => Some(EncodingKind::Format),
                            Some("framing") => Some(EncodingKind::Framing),
                            Some("handshake") => Some(EncodingKind::Handshake),
                            _ => None,
                        }
                    }
                    ("map", Value::Dictionary(languages)) => {
                        for language in &languages.assignments {
                            let Value::Dictionary(parts) = &language.value.value else {
                                continue;
                            };
                            let part = |name: &str| {
                                parts
                                    .assignments
                                    .iter()
                                    .find(|p| p.value.key.text() == name)
                                    .and_then(|p| text(&p.value.value))
                            };
                            if let Some(ty) = part("type") {
                                map.insert(
                                    language.value.key.text(),
                                    EncodingMapping {
                                        ty,
                                        package: part("package"),
                                        version: part("version"),
                                    },
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let Some(kind) = kind {
                declared.push(DeclaredEncoding {
                    name: entry.value.key.text(),
                    kind,
                    map,
                });
            }
        }
    }
    declared
}

/// What `config.idp` states about the wire, read as core reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredWire {
    /// `wire.format`, else core's default.
    pub format: String,
    /// `wire.framing`, else core's default.
    pub framing: String,
    /// `handshake`, by name, if the package states one.
    pub handshake: Option<String>,
}

impl Default for DeclaredWire {
    fn default() -> Self {
        use comline_core::schema::idl::annotations::{DEFAULT_FORMAT, DEFAULT_FRAMING};
        DeclaredWire {
            format: DEFAULT_FORMAT.to_string(),
            framing: DEFAULT_FRAMING.to_string(),
            handshake: None,
        }
    }
}

/// The wire `config.idp`'s text declares (`wire = { format = ... framing =
/// ... }`, `handshake`), core's defaults for what it leaves out or when it
/// doesn't interpret.
pub fn declared_wire(manifest: &str) -> DeclaredWire {
    use comline_core::package::config::ir::frozen;

    let Some(units) = declared_config(manifest) else {
        return DeclaredWire::default();
    };
    DeclaredWire {
        format: frozen::wire_format(&units).to_string(),
        framing: frozen::wire_framing(&units).to_string(),
        handshake: frozen::stated_handshake(&units).map(str::to_string),
    }
}

/// An encoding as declared, before its profile is read: name, kind and
/// map, and nothing the profile states.
pub fn bare_encoding(
    name: String,
    kind: comline_core::encodings::EncodingKind,
    map: comline_core::encodings::EncodingMap,
) -> comline_core::package::config::ir::frozen::FrozenUnit {
    comline_core::package::config::ir::frozen::FrozenUnit::Encoding {
        name,
        kind,
        profile_hash: String::new(),
        map,
        establishes: Vec::new(),
        has_vectors: false,
        grammar: None,
        calls: None,
        directions: None,
    }
}

/// `config.idp`'s text frozen as core freezes it, but for `settings` and
/// `encodings`, which need what the editor may not have (a dependency's
/// schemas, the profile files). What core's
/// `schema::ir::package_wire::apply_package_wire` takes. `None` when it
/// doesn't parse or interpret (mid-edit).
pub fn declared_config(
    manifest: &str,
) -> Option<Vec<comline_core::package::config::ir::frozen::FrozenUnit>> {
    use comline_core::package::config::idl::grammar::Key;
    use comline_core::package::config::ir::context::ProjectContext;
    use comline_core::package::config::ir::interpreter::interpret::interpret_context;

    let mut congregation = parser::parse_congregation(manifest)?;
    congregation
        .assignments
        .retain(|a| !matches!(&a.value.key, Key::Identifier(id) if id.value == "settings"));
    without_encodings(&mut congregation);
    interpret_context(&ProjectContext::with_config(congregation), None).ok()
}

/// Every encoding `config.idp` declares, frozen as core freezes it, with the
/// facts its profile states (`grammar`, `calls`, `directions`), read from
/// beside the manifest at `path`. Each entry is frozen on its own, so one
/// that doesn't (a missing profile, mid-edit, no `path`) costs only its own
/// facts: it's kept with its name, kind and suggested mappings alone, as
/// [`declared_encoding_details`] reads them, and states nothing.
pub fn declared_encoding_units(
    manifest: &str,
    path: Option<&std::path::Path>,
) -> Vec<comline_core::package::config::ir::frozen::FrozenUnit> {
    use comline_core::package::config::idl::grammar::Value;
    use comline_core::package::config::ir::context::{Origin, ProjectContext};
    use comline_core::package::config::ir::frozen::FrozenUnit as ConfigUnit;
    use comline_core::package::config::ir::interpreter::interpret::interpret_context;

    let frozen_one = |name: &str| -> Option<ConfigUnit> {
        let path = path?;
        let mut congregation = parser::parse_congregation(manifest)?;
        let mut encodings = congregation
            .assignments
            .iter()
            .find(|a| a.value.key.text() == "encodings")?
            .clone();
        let Value::Dictionary(entries) = &mut encodings.value.value else {
            return None;
        };
        entries.assignments.retain(|e| e.value.key.text() == name);
        congregation.assignments = vec![encodings];
        let context =
            ProjectContext::with_config_from_origin(Origin::Disk(path.to_path_buf()), congregation);
        let units = interpret_context(&context, None).ok()?;
        units
            .into_iter()
            .find(|u| matches!(u, ConfigUnit::Encoding { .. }))
    };
    declared_encoding_details(manifest)
        .into_iter()
        .map(|e| {
            frozen_one(&e.name).unwrap_or_else(|| bare_encoding(e.name, e.kind, e.map))
        })
        .collect()
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

    let congregation = parser::parse_congregation(manifest)?;
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

    if !declared_dependencies(manifest)
        .iter()
        .any(|d| d.name == dep_name)
    {
        return Err(format!("no dependency named `{dep_name}` is declared"));
    }

    let mut found: Option<(comline_core::settings::SettingsDict, String)> = None;
    for file in other_files {
        if file.dependency() != Some(dep_name) || !file.uri().path().ends_with(".ids") {
            continue;
        }
        let Some(document) = parser::parse_document(file.text()) else {
            continue;
        };
        let units = IncrementalInterpreter::from_declarations(document.0);
        for unit in &units {
            let SchemaFrozenUnit::Settings {
                name: Some(n),
                values,
                ..
            } = unit
            else {
                continue;
            };
            if n != block_name {
                continue;
            }
            let here = file
                .namespace()
                .map(|ns| ns.join("::"))
                .unwrap_or_else(|| file.uri().to_string());
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
                DeclaredDependency {
                    name: "hosted".into(),
                    kind: DependencyKind::Registry
                },
                DeclaredDependency {
                    name: "net".into(),
                    kind: DependencyKind::Git
                },
                DeclaredDependency {
                    name: "shared".into(),
                    kind: DependencyKind::Path("../shared".into())
                },
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
    fn declared_settings_is_the_built_in_default_with_no_settings_key() {
        use comline_core::settings::value::SettingsValue;

        let manifest = "congregation app\nspecification_version = 1\n";
        assert_eq!(
            declared_settings(manifest).get_path("annotations.framing.allowed"),
            Some(&SettingsValue::Bool(false)),
            "framing is forbidden by Comline's built-in default"
        );
    }

    #[test]
    fn declared_settings_is_the_built_in_base_mid_edit() {
        // Not empty: a half-typed manifest tells us nothing about the
        // package's own settings, but the built-in floor still applies, and
        // claiming otherwise made the editor disagree with every build.
        assert_eq!(
            declared_settings("congregation app\nsettings = {"),
            base_settings()
        );
    }

    #[test]
    fn declared_settings_skips_a_cross_package_reference() {
        // A reference needs dependency resolution, which this cannot do,
        // so it is recognised and skipped rather than interpreted. Core
        // used to *panic* on reaching that path; as of core#88 it returns
        // an error, but skipping it up front is still the right move - the
        // resolved value is `resolve_cross_package_settings`'s job.
        let manifest = "congregation app\nspecification_version = 1\n\
            settings = shared::settings::Strict\n";
        assert_eq!(declared_settings(manifest), base_settings());
    }

    #[test]
    fn a_malformed_package_id_is_what_a_build_would_reject() {
        let err = manifest_error(
            "congregation app\nid = \"nope\"\nspecification_version = 1\n",
            None,
        )
        .expect("a malformed id is an error");
        assert!(err.contains("hex"), "{err}");
    }

    #[test]
    fn manifest_error_names_what_a_build_would_reject() {
        // Silent in the editor before: this was a panic in core, so the
        // server died rather than reporting anything.
        let manifest = "congregation app\nspecification_version = 1\n\
            dependencies = {\n    std = {}\n}\n";
        let err = manifest_error(manifest, None).expect("a reserved dependency name is an error");
        assert!(err.contains("std"), "got: {err}");
        assert!(err.contains("reserved"), "got: {err}");
    }

    #[test]
    fn manifest_error_is_quiet_for_a_valid_or_incomplete_manifest() {
        assert_eq!(
            manifest_error("congregation app\nspecification_version = 1\n", None),
            None
        );
        // The package id (core#159) is a key the editor knows; a malformed
        // one is what a build refuses.
        let id = "congregation app\nid = \"0123456789abcdef0123456789abcdef\"\nspecification_version = 1\n";
        assert_eq!(manifest_error(id, None), None);
        // Incomplete: the parse pass already reports this, so reporting it
        // here too would double up.
        assert_eq!(manifest_error("congregation app\nsettings = {", None), None);
    }

    #[test]
    fn declared_encodings_reads_names_and_kinds() {
        use comline_core::encodings::EncodingKind;
        let manifest = "congregation app\nspecification_version = 1\n\
            encodings = {\n    \"com.acme.x@1\" = { kind = \"format\" profile = \"x.json\" }\n    \
            \"com.acme.y@1\" = { kind = \"nonsense\" profile = \"y.json\" }\n}\n";
        assert_eq!(
            declared_encodings(manifest),
            vec![("com.acme.x@1".to_string(), EncodingKind::Format)]
        );
        assert!(declared_encodings("congregation app\nencodings = {").is_empty());
    }

    #[test]
    fn a_manifest_declaring_encodings_still_says_what_else_it_declares() {
        // Freezing `encodings` reads each profile from the package
        // directory, which a manifest's text alone doesn't have; the rest
        // of the manifest is read regardless.
        let manifest = "congregation app\nspecification_version = 1\n\
            code_generation = {\n    languages = {\n        rust#1.85.0 = {}\n    }\n}\n\
            encodings = {\n    \"com.acme.x@1\" = { kind = \"format\" profile = \"x.json\" }\n}\n";
        assert_eq!(declared_languages(manifest).len(), 1);
        assert_eq!(manifest_error(manifest, None), None);
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

        let files = [dep_file(
            &["shared", "policy"],
            "settings Strict {\n    max_depth = 4\n}\n",
        )];
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
        assert!(
            err.contains("no dependency named `nonexistent`"),
            "got: {err}"
        );
    }

    #[test]
    fn a_reference_to_a_missing_block_is_an_error() {
        let files = [dep_file(
            &["shared", "policy"],
            "settings Other {\n    max_depth = 4\n}\n",
        )];
        let err = resolve_cross_package_settings(&consumer_manifest(), &files)
            .expect("a reference should resolve to Some")
            .expect_err("shared has no block named Strict");
        assert!(
            err.contains("no settings block named `Strict`"),
            "got: {err}"
        );
    }

    #[test]
    fn an_ambiguous_block_across_two_files_is_an_error() {
        let files = [
            dep_file(
                &["shared", "a"],
                "settings Strict {\n    max_depth = 4\n}\n",
            ),
            dep_file(
                &["shared", "b"],
                "settings Strict {\n    max_depth = 8\n}\n",
            ),
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

    #[test]
    fn a_bad_handshake_parameter_in_the_manifest_is_core_s_sentence_at_handshake() {
        let base = "congregation app\nspecification_version = 1\n";
        let good = format!("{base}handshake = {{ name = \"comline@2\" scope = \"package\" exchange = symmetric }}\n");
        assert_eq!(manifest_error(&good, None), None);
        for (written, says) in [
            (
                "scope = \"bogus\"",
                "handshake `comline@2`'s `scope` can't be `bogus`",
            ),
            (
                "speed = \"fast\"",
                "`speed` isn't a parameter of handshake `comline@2`",
            ),
        ] {
            let manifest = format!("{base}handshake = {{ name = \"comline@2\" {written} }}\n");
            let err = manifest_error(&manifest, None).expect(written);
            assert!(err.contains(says), "{err}");
            let (start, end) = manifest_error_span(&manifest, &err);
            assert!(
                manifest[start..end].starts_with("handshake = {"),
                "{:?}",
                &manifest[start..end]
            );
        }
        let manifest =
            format!("{base}handshake = {{ name = \"comline@1\" scope = \"package\" }}\n");
        let err = manifest_error(&manifest, None).expect("comline@1 takes none");
        assert!(err.contains("takes no parameters"), "{err}");
    }
}
