//! Real `use`-scoped cross-file symbol resolution, built on top of core's
//! [`ImportResolver::resolve_namespace`] and [`use_brings_into_scope`] - the
//! language server never re-derives either rule by hand.
//!
//! wasm32-safety note: [`namespace_of`] reads [`Url::path`], never
//! [`Url::to_file_path`] - the latter is gated to unix/windows/redox/wasi by
//! the `url` crate and doesn't exist on `wasm32-unknown-unknown`, which the
//! playground links this crate as (see `lib.rs`'s module doc). `url.path()`
//! is percent-encoded, which is fine here: a real namespace segment is an
//! identifier, never containing a character that would be encoded.

use std::collections::HashMap;
use std::path::Path;

use comline_core::package::layout;
use comline_core::schema::idl::grammar::{Declaration, Document};
use comline_core::schema::ir::compiler::import_resolver::{
    use_brings_into_scope, ImportResolver, ResolvedImport,
};
use lsp_types::{Position, Range, TextEdit, Url};

/// One project file the language server knows about - just enough to
/// resolve a `use` against it. `namespace` is the file's path under its
/// nearest `src/` ancestor, extension dropped (see [`namespace_of`]) - the
/// same derivation core uses on disk, so it agrees with a schema's real
/// namespace by construction.
pub struct ProjectFile<'a> {
    pub uri: &'a Url,
    pub source: &'a str,
    pub namespace: Vec<String>,
}

impl<'a> ProjectFile<'a> {
    pub fn new(uri: &'a Url, source: &'a str) -> Self {
        Self { uri, source, namespace: namespace_of(uri) }
    }
}

/// Derive a namespace from a file's URI: `layout::schemas_root_for` finds
/// the nearest `src/` ancestor, then `layout::namespace_for_schema_path`
/// turns the rest into namespace segments - identical to how core derives
/// one from a path on disk. Falls back to the file's own stem when there's
/// no `src/` ancestor at all (a loose, unsaved, or playground-virtual
/// file) - one segment, so it can still be matched by name, just never by
/// a multi-segment `use` path.
pub fn namespace_of(uri: &Url) -> Vec<String> {
    let path = Path::new(uri.path());

    if let Some(root) = layout::schemas_root_for(path) {
        if let Some(ns) = layout::namespace_for_schema_path(root, path) {
            return ns;
        }
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    vec![stem]
}

/// One resolved `use` (or legacy `import`) declaration: the namespace it
/// points at, its `as` alias if any, and the declaration's byte span in its
/// own file (so rename can edit the symbol name written in the `use` line).
pub struct ResolvedUse {
    pub resolved: ResolvedImport,
    pub alias: Option<String>,
    pub span: (usize, usize),
}

/// Every `use`/`import` in `document`, resolved against
/// `current_namespace`. `package_namespace: vec![]` and
/// `dependencies: HashMap::new()` are correct here, not a shortcut: schema
/// namespaces are `src`-relative and package-name-free throughout this
/// codebase (see core's `glob_schema_sources`/`interpret_schema_sources`),
/// so this agrees with the LSP's sibling namespaces by construction. Side
/// effect: `use crate::types::User` and `use types::User` become
/// indistinguishable here, matching real compiler behavior today. A real
/// external-dependency `use` simply won't match any sibling in
/// [`resolve_symbol`] - the LSP deliberately never reads `config.idp`, so
/// it has no dependency map to resolve one against; see that function's
/// fallback behavior for how a caller should treat a miss.
pub fn resolved_imports(document: &Document, current_namespace: &[String]) -> Vec<ResolvedUse> {
    let resolver = ImportResolver::new(vec![], HashMap::new(), None);

    document
        .0
        .iter()
        .filter_map(|decl| match &decl.value {
            Declaration::Use(use_stmt) => {
                let resolved = resolver.resolve_namespace(&use_stmt.path, current_namespace).ok()?;
                let alias = use_stmt.alias.as_ref().map(|a| a.name.text.clone());
                Some(ResolvedUse { resolved, alias, span: decl.span })
            }
            Declaration::Import(import) => {
                // The legacy `import pkg::Type` form - same shape as an
                // absolute `use`, just without `resolve_namespace`'s
                // self/parent/package handling (the legacy grammar rule
                // predates those prefixes entirely).
                let absolute_namespace: Vec<String> =
                    import.path().split("::").map(String::from).collect();
                Some(ResolvedUse {
                    resolved: ResolvedImport {
                        absolute_namespace,
                        schema_path: None,
                        symbols: vec![],
                        alias: None,
                    },
                    alias: None,
                    span: decl.span,
                })
            }
            _ => None,
        })
        .collect()
}

/// Resolve `name` against `imports` (the active file's own `use`/`import`
/// declarations) to the one sibling whose namespace a `use` actually
/// brings `name` into scope from. `None` means no `use` here resolves
/// `name` - not necessarily that `name` doesn't exist anywhere (the caller
/// decides what, if anything, to fall back to).
///
/// Longest-prefix match against each `use`'s resolved namespace, mirroring
/// core's own `find_schema_by_import_namespace_parts`: for `use
/// pkg::types::User`, this tries matching a sibling literally named
/// `pkg::types::User` first, then `pkg::types` (leaving `User` as the
/// remaining symbol - the common case), then `pkg`, in that order, so a
/// deeper sibling always wins over a shallower one with the same prefix.
pub fn resolve_symbol<'a>(
    name: &str,
    imports: &[ResolvedUse],
    siblings: &'a [ProjectFile<'a>],
) -> Option<ResolvedSymbol<'a>> {
    for use_decl in imports {
        let ns = &use_decl.resolved.absolute_namespace;
        let alias = use_decl.alias.as_deref();

        for split_at in (1..=ns.len()).rev() {
            let candidate_namespace = &ns[..split_at];
            let remaining = &ns[split_at..];

            let Some(sibling) = siblings.iter().find(|s| s.namespace == candidate_namespace)
            else {
                continue;
            };

            if use_brings_into_scope(&use_decl.resolved, remaining, alias, name) {
                // `remaining` is the symbol's real name inside `sibling` -
                // not necessarily `name` itself, when resolved through an
                // `as` alias (`use types::Message as Msg` + looking up
                // "Msg" must still find "Message" in `sibling`'s own
                // symbol table, which never heard of "Msg"). Empty
                // `remaining` means a whole-namespace/glob/multi import,
                // where the symbol genuinely is named `name` in `sibling`.
                let real_name =
                    if remaining.is_empty() { name.to_string() } else { remaining.join("::") };
                return Some(ResolvedSymbol { file: sibling, real_name });
            }
        }
    }

    None
}

/// What [`resolve_symbol`] found: which sibling, and the symbol's real
/// name there (see `real_name`'s doc on why that can differ from the name
/// `resolve_symbol` was called with).
pub struct ResolvedSymbol<'a> {
    pub file: &'a ProjectFile<'a>,
    pub real_name: String,
}

/// The edit adding `use <path>` to `source`: on the line after the last
/// `use`/`import`, or at the very top (plus a blank line) when there's none.
/// Works on the raw text, not the parsed tree, so completion can use it
/// mid-edit, when the file doesn't parse.
pub fn add_use_edit(source: &str, path: &str) -> TextEdit {
    let lines: Vec<&str> = source.lines().collect();
    let last_use = lines.iter().rposition(|line| {
        let line = line.trim_start();
        line.starts_with("use ") || line.starts_with("import ")
    });

    let (position, new_text) = match last_use {
        // The last line, with no newline after it to insert behind.
        Some(i) if i + 1 == lines.len() && !source.ends_with('\n') => {
            (Position::new(i as u32, lines[i].chars().count() as u32), format!("\nuse {path}"))
        }
        Some(i) => (Position::new(i as u32 + 1, 0), format!("use {path}\n")),
        None => (Position::new(0, 0), format!("use {path}\n\n")),
    };

    TextEdit { range: Range::new(position, position), new_text }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(namespace: &[&str]) -> ProjectFile<'static> {
        static URI: std::sync::OnceLock<Url> = std::sync::OnceLock::new();
        ProjectFile {
            uri: URI.get_or_init(|| Url::parse("file:///t.ids").unwrap()),
            source: "",
            namespace: namespace.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn parse_use(source: &str) -> comline_core::schema::idl::grammar::Document {
        comline_core::schema::idl::grammar::parse(source).expect("should parse")
    }

    #[test]
    fn namespace_of_a_nested_schema_path() {
        let uri = Url::parse("file:///pkg/src/chat/admin.ids").unwrap();
        assert_eq!(namespace_of(&uri), vec!["chat".to_string(), "admin".to_string()]);
    }

    #[test]
    fn namespace_of_a_rootless_file_falls_back_to_its_stem() {
        let uri = Url::parse("file:///tmp/scratch.ids").unwrap();
        assert_eq!(namespace_of(&uri), vec!["scratch".to_string()]);
    }

    #[test]
    fn use_with_remaining_symbol_resolves_to_the_namespace_sibling() {
        let doc = parse_use("use types::User");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        let found = resolve_symbol("User", &imports, &siblings).expect("should resolve");
        assert_eq!(found.file.namespace, vec!["types".to_string()]);
        assert_eq!(found.real_name, "User");
    }

    #[test]
    fn use_of_the_whole_namespace_resolves_any_name_it_declares() {
        let doc = parse_use("use types");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        assert!(resolve_symbol("AnyName", &imports, &siblings).is_some());
    }

    #[test]
    fn use_glob_resolves_any_name() {
        let doc = parse_use("use types::*");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        assert!(resolve_symbol("AnyName", &imports, &siblings).is_some());
    }

    #[test]
    fn use_multi_only_resolves_listed_names() {
        let doc = parse_use("use types::{User, Post}");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        assert!(resolve_symbol("User", &imports, &siblings).is_some());
        assert!(resolve_symbol("Comment", &imports, &siblings).is_none());
    }

    #[test]
    fn use_as_alias_resolves_under_both_the_alias_and_the_real_name() {
        // Matches `use_brings_into_scope`'s existing, already-shipped rule
        // (`symbol == name || alias == Some(name)`) - aliasing adds a
        // binding, it doesn't hide the original name.
        let doc = parse_use("use types::User as Account");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        assert!(resolve_symbol("Account", &imports, &siblings).is_some());
        assert!(resolve_symbol("User", &imports, &siblings).is_some());
    }

    #[test]
    fn resolving_through_an_alias_reports_the_real_name_not_the_alias() {
        // `types.ids`'s own symbol table never heard of "Account" - a
        // caller that looked it up there by the alias instead of
        // `real_name` would get nothing.
        let doc = parse_use("use types::Message as Msg");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        let found = resolve_symbol("Msg", &imports, &siblings).expect("should resolve");
        assert_eq!(found.real_name, "Message");
    }

    #[test]
    fn two_siblings_both_declaring_the_same_name_use_picks_the_imported_one() {
        // The actual regression this module exists to fix: a flat scan
        // can't tell these apart and would just return whichever sibling
        // happened to come first.
        let doc = parse_use("use b::Message");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["a"]), file(&["b"])];

        let found = resolve_symbol("Message", &imports, &siblings).expect("should resolve");
        assert_eq!(found.file.namespace, vec!["b".to_string()]);
    }

    #[test]
    fn self_parent_package_resolve_through_the_resolver_side_fix() {
        // `self`/`parent`/`package` parse as `UsePath::Absolute`, not
        // `UsePath::Relative` (see core's `try_resolve_relative_prefix`
        // doc) - `resolve_namespace` already compensates, so this needs
        // no special handling here at all.
        let doc = parse_use("use self::Sibling");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["app"])];

        let found = resolve_symbol("Sibling", &imports, &siblings).expect("should resolve");
        assert_eq!(found.file.namespace, vec!["app".to_string()]);
    }

    #[test]
    fn std_import_resolves_to_no_sibling_without_panicking() {
        let doc = parse_use("use std::collections::HashMap");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["app"])];

        assert!(resolve_symbol("HashMap", &imports, &siblings).is_none());
    }

    #[test]
    fn no_use_at_all_resolves_nothing() {
        let doc = parse_use("struct S { f: str }");
        let imports = resolved_imports(&doc, &["app".to_string()]);
        let siblings = vec![file(&["types"])];

        assert!(resolve_symbol("User", &imports, &siblings).is_none());
    }

    fn apply(source: &str, edit: &TextEdit) -> String {
        let offset = crate::util::position_to_offset(source, edit.range.start).unwrap_or(source.len());
        format!("{}{}{}", &source[..offset], edit.new_text, &source[offset..])
    }

    #[test]
    fn add_use_goes_after_the_last_existing_use() {
        let source = "use a::X\nuse b::Y\n\nstruct S {\n    m: Z\n}\n";
        assert_eq!(
            apply(source, &add_use_edit(source, "types::Z")),
            "use a::X\nuse b::Y\nuse types::Z\n\nstruct S {\n    m: Z\n}\n"
        );
    }

    #[test]
    fn add_use_goes_at_the_top_when_there_is_none() {
        let source = "struct S {\n    m: Z\n}\n";
        assert_eq!(apply(source, &add_use_edit(source, "types::Z")), "use types::Z\n\nstruct S {\n    m: Z\n}\n");
    }

    #[test]
    fn add_use_after_a_final_use_line_with_no_newline() {
        let source = "use a::X";
        assert_eq!(apply(source, &add_use_edit(source, "types::Z")), "use a::X\nuse types::Z");
    }
}
