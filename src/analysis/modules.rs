//! The modules of a project, for docs and navigation: a schema (`types`), a
//! directory of schemas (`api`, over `api/common.ids`), or a dependency's
//! package (`std`, `shared`). Hover and completion both read a module through
//! [`module`] and [`children`], so they can't disagree about what's in one.
//!
//! A schema documents itself with `//!` lines at its top, a package with the
//! same at the top of its `config.idp` (see `comline_core::schema::idl::
//! module_docs`); a directory with no schema of its own just lists what's in it.

use std::collections::{BTreeMap, HashMap};

use comline_core::schema::idl::grammar::{Declaration, ScopedIdentifier, UsePath};
use comline_core::schema::idl::vocabulary::{self, KeywordKind};
use comline_core::schema::ir::compiler::import_resolver::ImportResolver;
use lsp_types::Url;

use crate::analysis::imports;
use crate::analysis::project::{Project, ProjectDoc};

/// What a path segment names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// Its namespace as written from the project's root: `std::http`.
    pub namespace: Vec<String>,
    /// A dependency's package (`std`), as opposed to a module in one.
    pub package: bool,
    /// The dependency it's part of, `None` for the package being edited's own.
    pub dependency: Option<String>,
    /// Its `//!` docs, markdown.
    pub docs: Option<String>,
    /// The modules one level below it.
    pub modules: Vec<ChildModule>,
    /// What its schema declares, in order.
    pub declarations: Vec<Declared>,
}

/// A module one level below another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildModule {
    pub name: String,
    pub docs: Option<String>,
    /// A schema is right there, not only deeper ones: a path can end here.
    pub schema: bool,
    /// The dependency it belongs to, `None` for the package being edited's own.
    pub dependency: Option<String>,
}

/// One top-level declaration of a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub kind: DeclKind,
    pub name: String,
    /// Its `///` docstring.
    pub docs: Option<String>,
}

/// The kinds of declaration a schema can import from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Struct,
    Enum,
    Protocol,
    Const,
    Type,
    Error,
    Validator,
    Settings,
}

impl DeclKind {
    /// The keyword that declares it.
    pub fn keyword(self) -> &'static str {
        match self {
            DeclKind::Struct => "struct",
            DeclKind::Enum => "enum",
            DeclKind::Protocol => "protocol",
            DeclKind::Const => "const",
            DeclKind::Type => "type",
            DeclKind::Error => "error",
            DeclKind::Validator => "validator",
            DeclKind::Settings => "settings",
        }
    }
}

/// What the module at `namespace` is, or `None` when nothing in `project` has
/// that namespace (not a schema, not a directory of them, not a package).
pub fn module(project: &Project, namespace: &[String]) -> Option<Module> {
    let first = namespace.first()?;
    let package = namespace.len() == 1
        && (project.package_docs.contains_key(first) || project.docs.iter().any(|d| d.dependency == Some(first)));
    let schema = project.docs.iter().find(|d| d.namespace == namespace);
    let modules = children(project, namespace, false);
    if !package && schema.is_none() && modules.is_empty() {
        return None;
    }

    Some(Module {
        namespace: namespace.to_vec(),
        package,
        dependency: match package {
            true => Some(first.clone()),
            false => schema.and_then(|d| d.dependency).map(String::from),
        },
        docs: match package {
            true => project.package_docs.get(first).cloned(),
            false => schema.and_then(|d| d.docs.clone()),
        },
        modules,
        declarations: schema.map(declarations).unwrap_or_default(),
    })
}

/// The modules one level below `base` (`[]`: the top level), by name.
/// `local_only` leaves out dependencies' - what a relative path or a
/// package's own top level can reach.
pub fn children(project: &Project, base: &[String], local_only: bool) -> Vec<ChildModule> {
    struct Seen {
        schema: bool,
        dependency: Option<String>,
    }

    let mut found: BTreeMap<String, Seen> = BTreeMap::new();
    for doc in project.docs.iter().filter(|d| !local_only || d.dependency.is_none()) {
        if let Some(name) = child_of(&doc.namespace, base) {
            let seen = found.entry(name.to_string()).or_insert(Seen { schema: false, dependency: None });
            seen.schema |= doc.namespace.len() == base.len() + 1;
            seen.dependency = seen.dependency.take().or(doc.dependency.map(String::from));
        }
    }
    // Schemas that exist but don't parse right now: their names still count.
    for namespace in project.unparsed.iter().filter(|ns| !local_only || !is_dependency_root(project, ns.first())) {
        if let Some(name) = child_of(namespace, base) {
            let seen = found.entry(name.to_string()).or_insert(Seen { schema: false, dependency: None });
            seen.schema |= namespace.len() == base.len() + 1;
        }
    }

    found
        .into_iter()
        .map(|(name, seen)| {
            let mut namespace = base.to_vec();
            namespace.push(name.clone());
            let docs = project.docs.iter().find(|d| d.namespace == namespace).and_then(|d| d.docs.clone());
            ChildModule { name, docs, schema: seen.schema, dependency: seen.dependency }
        })
        .collect()
}

fn child_of<'n>(namespace: &'n [String], base: &[String]) -> Option<&'n str> {
    (namespace.len() > base.len() && namespace.starts_with(base)).then(|| namespace[base.len()].as_str())
}

fn is_dependency_root(project: &Project, first: Option<&String>) -> bool {
    first.is_some_and(|first| {
        project.dependencies.iter().any(|d| d.name == *first)
            || project.docs.iter().any(|d| d.dependency == Some(first.as_str()))
    })
}

/// What `doc` declares that a `use` can import - the same set core's
/// `check_imports` accepts - in the order written.
pub fn declarations(doc: &ProjectDoc) -> Vec<Declared> {
    doc.document
        .0
        .iter()
        .filter_map(|decl| {
            let (kind, name, docs) = match &decl.value {
                Declaration::Struct(s) => (DeclKind::Struct, s.name(), s.docstring()),
                Declaration::Enum(e) => (DeclKind::Enum, e.name(), e.docstring()),
                Declaration::Protocol(p) => (DeclKind::Protocol, p.name(), p.docstring()),
                Declaration::Const(c) => (DeclKind::Const, c.name(), c.docstring()),
                Declaration::TypeAlias(t) => (DeclKind::Type, t.name(), t.docstring()),
                Declaration::Error(e) => (DeclKind::Error, e.name(), e.docstring()),
                Declaration::Validator(v) => (DeclKind::Validator, v.name(), v.docstring()),
                Declaration::Settings(s) => (DeclKind::Settings, s.name(), s.docstring()),
                Declaration::Use(_) | Declaration::Import(_) => return None,
            };
            Some(Declared { kind, name, docs })
        })
        .collect()
}

/// The namespace `segments` name when written in the file at `uri`, the way
/// the build resolves a `use` path: `parent::common` is the sibling `common`.
/// `None` for a `parent::` above the top. The legacy `import` predates
/// relative prefixes, so its path is taken as written.
pub fn resolve_path(segments: &[String], legacy: bool, uri: &Url) -> Option<Vec<String>> {
    if legacy || !segments.first().is_some_and(|first| is_relative_prefix(first)) {
        return Some(segments.to_vec());
    }
    let path = UsePath::Absolute(ScopedIdentifier { text: format!("{}::_", segments.join("::")) });
    let resolver = ImportResolver::new(vec![], HashMap::new(), None);
    let mut namespace = resolver.resolve_namespace(&path, &imports::namespace_of(uri)).ok()?.absolute_namespace;
    namespace.pop();
    Some(namespace)
}

/// `self`, `parent` or `package`: a path prefix, not a module's name.
pub fn is_relative_prefix(segment: &str) -> bool {
    vocabulary::keyword(segment).is_some_and(|k| k.kind == KeywordKind::PathPrefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::source::SourceFile;

    const MANIFEST: &str = "congregation app\nspecification_version = 1\n\ndependencies = {\n    shared = {\n        path = \"../shared\"\n    }\n}\n";

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile::local(Url::parse(&format!("file:///pkg/{path}")).unwrap(), text.to_string())
    }

    fn dependency(path: &str, text: &str, namespace: &[&str]) -> SourceFile {
        SourceFile::of_dependency(
            Url::parse(&format!("file:///shared/{path}")).unwrap(),
            text.to_string(),
            "shared",
            namespace.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn files() -> Vec<SourceFile> {
        vec![
            file("config.idp", MANIFEST),
            file(
                "src/types.ids",
                "//! The package's own types.\n\n/// Someone.\nstruct User {\n    id: u64\n}\n\nenum Kind {\n    A\n}\n",
            ),
            file("src/api.ids", "//! The API.\n\nstruct Root {\n    id: u64\n}\n"),
            file("src/api/common.ids", "struct Error {\n    code: u32\n}\n"),
            file("src/api/v1/user.ids", "//! Users, v1.\n\nstruct Profile {\n    id: u64\n}\n"),
            file("src/broken.ids", "struct {"),
            dependency("config.idp", "//! A shared package.\n//!\n//! More.\ncongregation shared\nspecification_version = 1\n", &["shared"]),
            dependency("src/models.ids", "//! Shared models.\n\n/// A thing.\nstruct Thing {\n    id: u64\n}\n", &["shared", "models"]),
        ]
    }

    fn ns(path: &str) -> Vec<String> {
        path.split("::").map(String::from).collect()
    }

    #[test]
    fn a_schema_is_a_module_with_its_docs_and_declarations() {
        let files = files();
        let project = Project::new(files.iter());
        let types = module(&project, &ns("types")).unwrap();

        assert_eq!(types.docs.as_deref(), Some("The package's own types."));
        assert!(!types.package && types.dependency.is_none() && types.modules.is_empty());
        assert_eq!(
            types.declarations,
            vec![
                Declared { kind: DeclKind::Struct, name: "User".into(), docs: Some("Someone.".into()) },
                Declared { kind: DeclKind::Enum, name: "Kind".into(), docs: None },
            ]
        );
    }

    #[test]
    fn a_schema_over_a_directory_is_both() {
        let files = files();
        let project = Project::new(files.iter());
        let api = module(&project, &ns("api")).unwrap();

        assert_eq!(api.docs.as_deref(), Some("The API."));
        assert_eq!(api.declarations.len(), 1, "api.ids declares Root");
        let children: Vec<(&str, bool, Option<&str>)> =
            api.modules.iter().map(|m| (m.name.as_str(), m.schema, m.docs.as_deref())).collect();
        assert_eq!(children, [("common", true, None), ("v1", false, None)]);
    }

    #[test]
    fn a_directory_without_a_schema_just_lists_what_is_in_it() {
        let files = files();
        let project = Project::new(files.iter());
        let v1 = module(&project, &ns("api::v1")).unwrap();

        assert_eq!(v1.docs, None);
        assert!(v1.declarations.is_empty());
        assert_eq!(v1.modules.len(), 1);
        assert_eq!(v1.modules[0].name, "user");
        assert_eq!(v1.modules[0].docs.as_deref(), Some("Users, v1."), "the child schema's own docs");
    }

    #[test]
    fn a_dependency_is_a_package_documented_by_its_manifest() {
        let files = files();
        let project = Project::new(files.iter());
        let shared = module(&project, &ns("shared")).unwrap();

        assert!(shared.package);
        assert_eq!(shared.dependency.as_deref(), Some("shared"));
        assert_eq!(shared.docs.as_deref(), Some("A shared package.\n\nMore."));
        assert_eq!(shared.modules.len(), 1);
        assert_eq!(shared.modules[0].docs.as_deref(), Some("Shared models."));

        let models = module(&project, &ns("shared::models")).unwrap();
        assert!(!models.package);
        assert_eq!(models.dependency.as_deref(), Some("shared"));
        assert_eq!(models.declarations[0].name, "Thing");
    }

    #[test]
    fn a_dependencys_manifest_does_not_replace_the_packages_own() {
        let files = files();
        let project = Project::new(files.iter());
        assert!(project.has_manifest);
        assert_eq!(project.dependencies.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["shared"]);
        assert_eq!(project.package_docs.keys().collect::<Vec<_>>(), ["shared"]);
    }

    #[test]
    fn unknown_namespaces_and_the_root_are_not_modules() {
        let files = files();
        let project = Project::new(files.iter());
        assert_eq!(module(&project, &ns("nope")), None);
        assert_eq!(module(&project, &ns("shared::nope")), None);
        assert_eq!(module(&project, &[]), None);
    }

    #[test]
    fn a_schema_that_doesnt_parse_still_counts_as_a_child() {
        let files = files();
        let project = Project::new(files.iter());
        let top = children(&project, &[], true);
        let names: Vec<&str> = top.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["api", "broken", "types"], "no dependencies when local only");
        assert!(top.iter().find(|m| m.name == "broken").unwrap().schema);
    }

    #[test]
    fn relative_paths_resolve_like_the_build() {
        let uri = Url::parse("file:///pkg/src/api/v1/user.ids").unwrap();
        assert_eq!(resolve_path(&ns("parent"), false, &uri), Some(ns("api::v1")));
        assert_eq!(resolve_path(&ns("parent::common"), false, &uri), Some(ns("api::v1::common")));
        assert_eq!(resolve_path(&ns("self"), false, &uri), Some(ns("api::v1::user")));
        assert_eq!(resolve_path(&ns("package"), false, &uri), Some(vec![]));
        assert_eq!(resolve_path(&ns("shared::models"), false, &uri), Some(ns("shared::models")));
        assert_eq!(resolve_path(&ns("parent"), true, &uri), Some(ns("parent")), "`import` has no prefixes");
        let top = Url::parse("file:///pkg/src/types.ids").unwrap();
        assert_eq!(resolve_path(&ns("parent"), false, &top), Some(vec![]), "one up from a top-level schema");
        // Like the build: only the leading segment is a prefix, a second `parent`
        // is a module's name.
        assert_eq!(resolve_path(&ns("parent::parent"), false, &top), Some(ns("parent")));
    }
}
