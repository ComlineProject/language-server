//! What a type name written in a schema names: core's rule, over the view.
//!
//! A qualified reference follows the Rust model (core#290): a name in scope is
//! a declaration of the schema, what a `use` of it brings in (its alias, else
//! its last segment) or a root (`package`, `parent`, `self`, `std`, a
//! dependency's name). A whole-namespace `use` (`use package::types`) brings in
//! only `types`, and a full path from a root needs no `use` at all. The rule is
//! core's [`Scope`], run here over the files the server has: [`Names`] builds
//! the [`ProjectContext`] core reads (each file's declarations and text under
//! its namespace, the dependencies the manifest declares) and asks it, so the
//! editor refuses and resolves exactly the names `comline build` does.
//!
//! A [`ProjectContext`] holds `Rc`s, so it can't live in the [`Project`] a
//! server keeps across `await`s; a [`Names`] is built for one request, and
//! dropped with it.
//!
//! A qualified type freezes as its path from the package's root
//! (`api::Invoice` after `use package::billing::api`, an alias's
//! `b::Invoice` and `package::billing::api::Invoice` are all
//! `billing::api::Invoice`), which is what core's validation reads and what
//! [`Names::freeze`] writes into a file's units.

use std::collections::BTreeSet;

use comline_core::diagnostics::Diagnostic as CoreDiagnostic;
use comline_core::package::config::idl::grammar as config_grammar;
use comline_core::package::config::ir::context::ProjectContext;
use comline_core::schema::ir::compiler::references::{check_references, Refusal, Resolved, Scope};
use comline_core::schema::ir::context::SchemaContext;
use comline_core::schema::ir::frozen::types::TypeRef;
use comline_core::schema::ir::frozen::unit::FrozenUnit;
use comline_core::utils::codemap::CodeMap;

use crate::analysis::import_check::importable_names;
use crate::analysis::imports::ResolvedUse;
use crate::analysis::project::{Project, Target};
use crate::analysis::stdlib;
use crate::analysis::symbols::Namespace;

/// The names of a project, as core's reference rule reads them.
pub struct Names<'p, 'a> {
    project: &'p Project<'a>,
    context: ProjectContext,
}

impl<'p, 'a> Names<'p, 'a> {
    /// `project`'s schemas (each with its declarations and its text, under
    /// the namespace it is seen as) and the dependencies it names.
    pub fn of(project: &'p Project<'a>) -> Self {
        let congregation = config_grammar::parse(&manifest(project))
            .or_else(|_| config_grammar::parse("congregation package"))
            .expect("a bare congregation parses");
        let mut context = ProjectContext::with_config(congregation);
        let schema = |declarations, namespace: &[String], codemap| {
            let schema =
                SchemaContext::with_declarations(declarations, namespace.to_vec(), codemap);
            std::rc::Rc::new(std::cell::RefCell::new(schema))
        };
        for doc in &project.docs {
            let mut codemap = CodeMap::new();
            codemap.insert_file(doc.namespace.join("/"), doc.source);
            context
                .schema_contexts
                .push(schema(doc.document.0.clone(), &doc.namespace, codemap));
        }
        // They exist, so a path to one is no unknown path; what they declare
        // isn't known.
        for namespace in &project.unparsed {
            context
                .schema_contexts
                .push(schema(vec![], namespace, CodeMap::new()));
        }
        Self { project, context }
    }

    /// The project these are the names of.
    pub fn project(&self) -> &'p Project<'a> {
        self.project
    }

    /// The names in scope in `docs[doc]`.
    pub fn scope(&self, doc: usize) -> Scope<'_> {
        let doc = &self.project.docs[doc];
        Scope::new(&doc.document.0, &doc.namespace, &self.context)
    }

    /// What `written`, a type name in `docs[doc]`, names, or why nothing.
    pub fn resolve(&self, doc: usize, written: &str) -> Result<Resolved, Refusal> {
        self.scope(doc).resolve(written)
    }

    /// The declaration `written`, a type name in `docs[doc]`, names, when the
    /// project holds it.
    pub fn target(&self, doc: usize, written: &str) -> Option<Target> {
        self.target_in(&self.scope(doc), written)
    }

    /// [`Names::target`], in a scope already built.
    pub fn target_in(&self, scope: &Scope<'_>, written: &str) -> Option<Target> {
        let (namespace, name) = scope.resolve(written).ok()?.target?;
        self.target_of(&namespace, &name)
    }

    /// The symbol `name` of the schema `namespace`.
    fn target_of(&self, namespace: &[String], name: &str) -> Option<Target> {
        let doc = self
            .project
            .docs
            .iter()
            .position(|d| d.namespace == namespace)?;
        let symbol = self.project.docs[doc]
            .symbols
            .index_in(name, Namespace::Declaration)?;
        Some(Target { doc, symbol })
    }

    /// The schema (an index into the project's files) and the name in it that
    /// `name`, as a unit of `docs[doc]` holds it, stands for: a qualified type
    /// is frozen as its path from the package's root, so it names its schema
    /// outright; anything else is read through the file's scope, and for the
    /// names a `use` is still read for in the older, wider way (a validator a
    /// whole-namespace `use` brought in), through that.
    pub fn declaration(&self, doc: usize, name: &str) -> Option<(usize, String)> {
        if let Some(found) = self.by_path(name) {
            return Some(found);
        }
        if let Ok(Resolved {
            target: Some((namespace, real)),
            ..
        }) = self.scope(doc).resolve(name)
        {
            let index = self
                .project
                .docs
                .iter()
                .position(|d| d.namespace == namespace)?;
            return Some((index, real));
        }
        let bare = name.rsplit("::").next()?;
        let target = self
            .project
            .resolve_wide(doc, bare, Namespace::Declaration)?;
        let real = self.project.symbol(&target)?.name.clone();
        Some((target.doc, real))
    }

    /// The schema whose namespace is `name`'s qualifier, when it declares it:
    /// the way a frozen qualified type is read.
    fn by_path(&self, name: &str) -> Option<(usize, String)> {
        let (qualifier, bare) = name.rsplit_once("::")?;
        let parts: Vec<String> = qualifier.split("::").map(String::from).collect();
        let index = self
            .project
            .docs
            .iter()
            .position(|d| d.namespace == parts)?;
        importable_names(&self.project.docs[index])
            .contains(bare)
            .then(|| (index, bare.to_string()))
    }

    /// Whether `name`, a qualified type as a unit holds it, is a path to a
    /// declaration the project holds (core's `ImportLookup::reaches`): in a
    /// schema that doesn't parse right now, taken as declared.
    pub fn reaches(&self, name: &str) -> bool {
        if self.by_path(name).is_some() {
            return true;
        }
        let Some((qualifier, _)) = name.rsplit_once("::") else {
            return false;
        };
        self.project
            .unparsed
            .iter()
            .any(|namespace| namespace.join("::") == qualifier)
    }

    /// `units` of `docs[doc]` with every qualified type named by its path from
    /// the package's root, as core's compiler freezes it.
    pub fn freeze(&self, doc: usize, units: &mut [FrozenUnit]) {
        let scope = self.scope(doc);
        for unit in units {
            unit.for_each_type_mut(&mut |ty| {
                *ty = ty.map_named(&mut |name| scope.frozen(name).map(TypeRef::Named));
            });
        }
    }

    /// The errors core's [`check_references`] reports for `docs[doc]`: a name
    /// nothing brought in, a qualifier that isn't in scope, a name defined
    /// twice, each with its fix. Every one, in source order.
    pub fn reference_errors(&self, doc: usize) -> Vec<CoreDiagnostic> {
        let doc = &self.project.docs[doc];
        check_references(&doc.document.0, &doc.namespace, &self.context)
            .err()
            .unwrap_or_default()
    }

    /// The namespaces `docs[doc]` writes a type path into, for what a hover
    /// reaches (a full path needs no `use`).
    pub fn written_namespaces(&self, doc: usize) -> Vec<Vec<String>> {
        let scope = self.scope(doc);
        let declarations = &self.project.docs[doc].document.0;
        let mut found: Vec<Vec<String>> = vec![];
        for written in comline_core::schema::ir::compiler::references::named_types(declarations) {
            if let Some(namespace) = scope.reaches(&written.name) {
                if !found.contains(&namespace) {
                    found.push(namespace);
                }
            }
        }
        found
    }
}

/// A manifest for core to read the dependencies from: those `config.idp`
/// declares, and any whose files the view holds.
fn manifest(project: &Project) -> String {
    let mut names: BTreeSet<&str> = project
        .dependencies
        .iter()
        .map(|d| d.name.as_str())
        .chain(project.docs.iter().filter_map(|d| d.dependency))
        .collect();
    // Not a dependency's name: `std` is the library's own, and core refuses
    // the prefixes.
    names.retain(|name| {
        *name != stdlib::NAME && !crate::analysis::modules::is_relative_prefix(name)
    });

    let mut text = String::from("congregation package\n");
    if !names.is_empty() {
        text.push_str("\ndependencies = {\n");
        for name in names {
            text.push_str(&format!("    {name} = {{\n        path = \".\"\n    }}\n"));
        }
        text.push_str("}\n");
    }
    text
}

/// A name a whole-namespace `use` brings in (`use package::types`,
/// `use package::billing::api as b`), and the namespace it stands for: to
/// qualify with, not a type. From `uses`, the `use`s as written, so it reads
/// a file that doesn't parse as well as one that does.
pub fn namespace_bindings(project: &Project, uses: &[ResolvedUse]) -> Vec<(String, Vec<String>)> {
    uses.iter()
        .filter_map(|u| {
            let namespace = &u.resolved.absolute_namespace;
            if !u.resolved.symbols.is_empty() {
                return None;
            }
            let name = u.alias.clone().or_else(|| namespace.last().cloned())?;
            is_namespace(project, namespace).then(|| (name, namespace.clone()))
        })
        .collect()
}

/// Whether `namespace` is a schema or a directory of them, `std` or a
/// dependency's name: something to qualify with.
pub(crate) fn is_namespace(project: &Project, namespace: &[String]) -> bool {
    if let [only] = namespace {
        if only == stdlib::NAME || project.dependencies.iter().any(|d| d.name == *only) {
            return true;
        }
    }
    !namespace.is_empty()
        && project
            .docs
            .iter()
            .map(|d| &d.namespace)
            .chain(&project.unparsed)
            .any(|schema| schema.starts_with(namespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Url;

    fn files(list: &[(&str, &str)]) -> Vec<(Url, String)> {
        list.iter()
            .map(|(uri, text)| (Url::parse(uri).unwrap(), text.to_string()))
            .collect()
    }

    const MANIFEST: &str = "congregation app\nspecification_version = 1\n\ndependencies = {\n    shared = {\n        path = \"../shared\"\n    }\n}\n";

    fn package() -> Vec<(Url, String)> {
        files(&[
            ("file:///pkg/config.idp", MANIFEST),
            (
                "file:///pkg/src/types.ids",
                "struct User {\n    id: u64\n}\n",
            ),
            (
                "file:///pkg/src/billing/api.ids",
                "struct Invoice {\n    id: u64\n}\n",
            ),
            (
                "file:///pkg/src/app.ids",
                "struct Local {\n    id: u64\n}\n",
            ),
        ])
    }

    fn app(source: &str) -> Vec<(Url, String)> {
        let mut all = package();
        all.retain(|(uri, _)| !uri.path().ends_with("app.ids"));
        all.push((
            Url::parse("file:///pkg/src/app.ids").unwrap(),
            source.to_string(),
        ));
        all
    }

    fn resolved(source: &str, written: &str) -> Option<(Vec<String>, String)> {
        let all = app(source);
        let project = Project::new(all.iter());
        let doc = project.index_of(&Url::parse("file:///pkg/src/app.ids").unwrap())?;
        Names::of(&project).resolve(doc, written).ok()?.target
    }

    fn ns(path: &str) -> Vec<String> {
        path.split("::").map(String::from).collect()
    }

    #[test]
    fn a_whole_namespace_use_brings_in_only_its_name() {
        let source = "use package::types\n\nstruct S {\n    u: types::User\n}\n";
        assert_eq!(
            resolved(source, "types::User"),
            Some((ns("types"), "User".into()))
        );
        assert_eq!(
            resolved(source, "User"),
            None,
            "nothing in `types` is in scope bare"
        );
    }

    #[test]
    fn a_full_path_from_a_root_needs_no_use() {
        let source = "struct S {\n    u: package::types::User\n}\n";
        assert_eq!(
            resolved(source, "package::types::User"),
            Some((ns("types"), "User".into()))
        );
    }

    #[test]
    fn a_path_goes_down_from_a_name_in_scope_and_through_an_alias() {
        let down = "use package::billing\n\nstruct S {\n    i: billing::api::Invoice\n}\n";
        assert_eq!(
            resolved(down, "billing::api::Invoice"),
            Some((ns("billing::api"), "Invoice".into()))
        );
        let alias = "use package::billing::api as b\n\nstruct S {\n    i: b::Invoice\n}\n";
        assert_eq!(
            resolved(alias, "b::Invoice"),
            Some((ns("billing::api"), "Invoice".into()))
        );
    }

    #[test]
    fn after_an_alias_the_namespace_s_own_name_is_not_in_scope() {
        let source = "use package::types as t\n\nstruct S {\n    u: types::User\n}\n";
        assert_eq!(resolved(source, "types::User"), None);
        assert_eq!(
            resolved(source, "t::User"),
            Some((ns("types"), "User".into()))
        );
    }

    #[test]
    fn a_qualified_type_freezes_as_its_path_and_reaches_what_it_names() {
        let all = app("use package::billing::api as b\n\nstruct S {\n    i: b::Invoice\n}\n");
        let project = Project::new(all.iter());
        let doc = project
            .index_of(&Url::parse("file:///pkg/src/app.ids").unwrap())
            .unwrap();
        let names = Names::of(&project);
        assert_eq!(
            names.scope(doc).frozen("b::Invoice").as_deref(),
            Some("billing::api::Invoice")
        );
        assert!(names.reaches("billing::api::Invoice"));
        assert!(!names.reaches("billing::api::Nope"));
        assert!(!names.reaches("billing::Invoice"));
    }

    #[test]
    fn a_namespace_use_is_a_binding_and_an_item_use_is_not() {
        let all = app("use package::types\nuse package::billing::api::Invoice\nuse package::billing::api as b\n");
        let project = Project::new(all.iter());
        let doc = project
            .index_of(&Url::parse("file:///pkg/src/app.ids").unwrap())
            .unwrap();
        assert_eq!(
            namespace_bindings(&project, &project.docs[doc].imports),
            vec![
                ("types".to_string(), ns("types")),
                ("b".to_string(), ns("billing::api")),
            ]
        );
    }
}
