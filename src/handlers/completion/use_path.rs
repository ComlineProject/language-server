//! Completion inside a `use` path. `use ` offers what a path can start with
//! (the package's namespaces, its dependencies, `self` / `parent` /
//! `package`, `std`), `use shared::` what's under `shared`, and
//! `use shared::models::{Thing, ` the rest of what `shared::models` declares.
//!
//! A path is resolved the way the build resolves it, relative prefixes
//! through core's own `ImportResolver`, so what's offered is what
//! `comline check` accepts.

use std::collections::{BTreeMap, HashMap};

use comline_core::schema::idl::grammar::{Declaration, ScopedIdentifier, UsePath};
use comline_core::schema::idl::vocabulary::{self, KeywordKind};
use comline_core::schema::ir::compiler::import_resolver::ImportResolver;
use lsp_types::{
    Command, CompletionItem, CompletionItemKind, CompletionTextEdit, InsertTextFormat, Range, TextEdit, Url,
};

use crate::analysis::imports;
use crate::analysis::project::{Project, ProjectDoc};
use crate::analysis::source::{DependencyKind, ProjectSource};
use crate::analysis::stdlib;
use crate::util::byte_range_to_lsp_range;

/// How many lines up a `{ ... }` list is followed, looking for its `use`.
const MAX_LIST_LINES: usize = 64;

/// Where the cursor is in a `use` statement.
#[derive(Debug, PartialEq)]
pub(super) enum UseContext {
    /// Typing a path segment, or an item inside `{ ... }`.
    Path(UsePrefix),
    /// Somewhere in a `use` statement with nothing to offer: naming an `as`
    /// alias, past the end of the path, between the two `:` of `::`.
    Nothing,
}

/// What's written of a `use` path before the cursor.
#[derive(Debug, PartialEq)]
pub(super) struct UsePrefix {
    /// The legacy `import`, which takes only a plain path.
    pub legacy: bool,
    /// The segments before the one being typed, as written.
    pub segments: Vec<String>,
    /// Inside `{ ... }`: the items listed so far.
    pub listed: Option<Vec<String>>,
    /// Byte offset where the segment or item being typed starts.
    pub partial_start: usize,
}

/// The `use` statement the cursor at `offset` is in, read from the raw text
/// (a file being edited rarely parses). `None` outside any `use` or `import`.
pub(super) fn context_at(source: &str, offset: usize) -> Option<UseContext> {
    let (statement, legacy) = statement_before(source, offset)?;
    Some(parse_statement(&statement, legacy, offset))
}

/// The `use` statement's text up to `offset`, from its keyword; and whether
/// it's the legacy `import`. A statement is the cursor's own line, or a
/// `{ ... }` list still open over several lines.
fn statement_before(source: &str, offset: usize) -> Option<(String, bool)> {
    let mut start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    // The cursor's line first (no comment before the cursor: the caller
    // already returned for one), then each line above it, comment cut.
    let mut lines = vec![&source[start..offset]];

    for _ in 0..MAX_LIST_LINES {
        let line = lines[lines.len() - 1].trim_start();
        if let Some(legacy) = keyword(line) {
            let statement = lines.iter().rev().copied().collect::<Vec<_>>().join("\n");
            let open_list = statement.contains('{') && !statement.contains('}');
            return (lines.len() == 1 || open_list).then(|| (statement.trim_start().to_string(), legacy));
        }
        if start == 0 || !is_list_line(line) {
            return None;
        }
        let end = start - 1;
        start = source[..end].rfind('\n').map_or(0, |i| i + 1);
        lines.push(cut_comment(&source[start..end]));
    }
    None
}

fn parse_statement(statement: &str, legacy: bool, offset: usize) -> UseContext {
    let keyword_len = if legacy { "import".len() } else { "use".len() };
    let body = statement[keyword_len..].trim_start();

    if let Some(brace) = body.find('{') {
        let inside = &body[brace + 1..];
        let base = body[..brace].trim_end().strip_suffix("::").and_then(segments_of);
        // `import` takes no list; a closed list is a finished path.
        let Some(segments) = base.filter(|_| !legacy && !inside.contains('}')) else {
            return UseContext::Nothing;
        };
        let mut items: Vec<&str> = inside.split(',').collect();
        let partial = items.pop().unwrap_or_default().trim_start();
        if !is_partial_identifier(partial) {
            return UseContext::Nothing;
        }
        let listed = items.into_iter().map(str::trim).filter(|i| !i.is_empty()).map(String::from).collect();
        let partial_start = offset - partial.len();
        return UseContext::Path(UsePrefix { legacy, segments, listed: Some(listed), partial_start });
    }

    // Whitespace ends the path: what follows is `as`, or nothing.
    if body.contains(char::is_whitespace) {
        return UseContext::Nothing;
    }
    let (finished, partial) = body.rsplit_once("::").unwrap_or(("", body));
    let segments = if finished.is_empty() { Some(vec![]) } else { segments_of(finished) };
    match segments {
        Some(segments) if is_partial_identifier(partial) => {
            UseContext::Path(UsePrefix { legacy, segments, listed: None, partial_start: offset - partial.len() })
        }
        _ => UseContext::Nothing,
    }
}

/// `Some(legacy)` when `line` starts with `use` or `import` and a space.
fn keyword(line: &str) -> Option<bool> {
    [("use", false), ("import", true)].into_iter().find_map(|(word, legacy)| {
        line.strip_prefix(word).filter(|rest| rest.starts_with(char::is_whitespace)).map(|_| legacy)
    })
}

/// A line that can sit inside a `{ ... }` list: names, commas, blanks.
fn is_list_line(line: &str) -> bool {
    line.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ',' || c.is_whitespace())
}

/// `line` without its `//` comment. A `use` statement has no strings, so a
/// `//` is always one.
fn cut_comment(line: &str) -> &str {
    line.find("//").map_or(line, |i| &line[..i])
}

fn segments_of(path: &str) -> Option<Vec<String>> {
    path.split("::").map(|s| is_identifier(s).then(|| s.to_string())).collect()
}

fn is_identifier(s: &str) -> bool {
    s.starts_with(|c: char| c.is_alphabetic() || c == '_') && is_partial_identifier(s)
}

fn is_partial_identifier(s: &str) -> bool {
    s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// What can come next in the path `prefix` describes, written in the file at
/// `uri`, with `other_files` the rest of its package.
pub(super) fn completions<S: ProjectSource>(
    prefix: &UsePrefix,
    source: &str,
    uri: &Url,
    offset: usize,
    other_files: &[S],
) -> Vec<CompletionItem> {
    let project = Project::new(other_files.iter());
    let range = byte_range_to_lsp_range(source, prefix.partial_start, offset);

    if prefix.segments.is_empty() {
        return path_starts(&project, prefix.legacy, range);
    }

    let relative = is_relative_prefix(&prefix.segments[0]);
    let Some(base) = resolve(&prefix.segments, prefix.legacy, uri) else {
        return vec![]; // `parent::` above the top
    };
    // A relative path stays inside the package.
    let docs: Vec<&ProjectDoc> = project.docs.iter().filter(|d| !relative || d.dependency.is_none()).collect();
    let schema = docs.iter().copied().find(|d| d.namespace == base);

    if let Some(listed) = &prefix.listed {
        let Some(schema) = schema else { return vec![] };
        return declarations(schema, range).filter(|item| !listed.contains(&item.label)).collect();
    }

    let mut items = children(&docs, &project.unparsed, &base, range);
    if let Some(schema) = schema {
        items.extend(declarations(schema, range));
        // `use parent::*` and `use self::{A}` don't parse: a prefix alone
        // before `::*` or `::{` lexes as its keyword.
        let bare_prefix = relative && prefix.segments.len() == 1;
        if !prefix.legacy && !bare_prefix {
            items.extend(glob_and_list(&base, range));
        }
    }
    items
}

/// `use ` itself: the package's own top-level namespaces, then its declared
/// dependencies, then `self` / `parent` / `package` and `std`.
fn path_starts(project: &Project, legacy: bool, range: Range) -> Vec<CompletionItem> {
    let is_dependency = |name: &str| project.dependencies.iter().any(|d| d.name == name);
    let local: Vec<&ProjectDoc> = project.docs.iter().filter(|d| d.dependency.is_none()).collect();
    let unparsed: Vec<Vec<String>> =
        project.unparsed.iter().filter(|ns| !ns.first().is_some_and(|first| is_dependency(first))).cloned().collect();

    let mut items: Vec<CompletionItem> = children(&local, &unparsed, &[], range)
        .into_iter()
        .map(|item| sorted(item, '0'))
        .collect();

    for dependency in &project.dependencies {
        if items.iter().any(|item| item.label == dependency.name) {
            continue;
        }
        let indexed = project.docs.iter().any(|d| d.dependency == Some(dependency.name.as_str()));
        let detail = match (&dependency.kind, indexed) {
            (DependencyKind::Path(path), true) => format!("dependency, path `{path}`"),
            (DependencyKind::Path(path), false) => format!("dependency, path `{path}` — no schemas there"),
            (DependencyKind::Git, true) => "dependency, git".to_string(),
            (DependencyKind::Git, false) => "dependency, git — not fetched yet".to_string(),
            (DependencyKind::Registry, _) => "dependency, registry — not supported yet".to_string(),
        };
        items.push(sorted(segment_item(&dependency.name, detail, CompletionItemKind::MODULE, true, range), '1'));
    }

    if !legacy {
        for prefix in vocabulary::keywords_of_kind(KeywordKind::PathPrefix) {
            let item = segment_item(prefix.text, prefix.description.to_string(), CompletionItemKind::KEYWORD, true, range);
            items.push(sorted(item, '2'));
        }
        let std = segment_item(stdlib::NAME, "the standard library".to_string(), CompletionItemKind::MODULE, true, range);
        items.push(sorted(std, '3'));
    }
    items
}

/// The next segment under `base`: every namespace one level below it, from
/// `docs` and the schemas that don't parse right now.
fn children(docs: &[&ProjectDoc], unparsed: &[Vec<String>], base: &[String], range: Range) -> Vec<CompletionItem> {
    #[derive(Default)]
    struct Child<'a> {
        /// A schema is right there, not only deeper ones.
        schema: bool,
        dependency: Option<&'a str>,
    }

    let mut found: BTreeMap<&str, Child> = BTreeMap::new();
    for doc in docs {
        if let Some(name) = child_of(&doc.namespace, base) {
            let child = found.entry(name).or_default();
            child.schema |= doc.namespace.len() == base.len() + 1;
            child.dependency = child.dependency.or(doc.dependency);
        }
    }
    for namespace in unparsed {
        if let Some(name) = child_of(namespace, base) {
            found.entry(name).or_default().schema |= namespace.len() == base.len() + 1;
        }
    }

    found
        .into_iter()
        .map(|(name, child)| {
            let mut detail = match child.schema {
                true => format!("`{name}.ids`"),
                false => format!("`{name}/`"),
            };
            if let Some(dependency) = child.dependency {
                detail.push_str(&format!(" ({})", stdlib::owner(dependency)));
            }
            // A path can end at a schema; a directory always goes on.
            segment_item(name, detail, CompletionItemKind::MODULE, !child.schema, range)
        })
        .collect()
}

fn child_of<'n>(namespace: &'n [String], base: &[String]) -> Option<&'n str> {
    (namespace.len() > base.len() && namespace.starts_with(base)).then(|| namespace[base.len()].as_str())
}

/// What `doc` declares that a `use` can import - the same set core's
/// `check_imports` accepts.
fn declarations<'d>(doc: &'d ProjectDoc, range: Range) -> impl Iterator<Item = CompletionItem> + 'd {
    doc.document.0.iter().filter_map(move |decl| {
        let (name, kind, keyword) = match &decl.value {
            Declaration::Struct(s) => (&s.name.text, CompletionItemKind::STRUCT, "struct"),
            Declaration::Enum(e) => (&e.name.text, CompletionItemKind::ENUM, "enum"),
            Declaration::Protocol(p) => (&p.name.text, CompletionItemKind::INTERFACE, "protocol"),
            Declaration::Const(c) => (&c.name.text, CompletionItemKind::CONSTANT, "const"),
            Declaration::TypeAlias(t) => (&t.name.text, CompletionItemKind::CLASS, "type alias"),
            Declaration::Error(e) => (&e.name.text, CompletionItemKind::EVENT, "error"),
            Declaration::Validator(v) => (&v.name.text, CompletionItemKind::FUNCTION, "validator"),
            Declaration::Settings(s) => (&s.name.text, CompletionItemKind::PROPERTY, "settings"),
            Declaration::Use(_) | Declaration::Import(_) => return None,
        };
        Some(segment_item(name, keyword.to_string(), kind, false, range))
    })
}

/// `*` and `{…}` after a schema's path.
fn glob_and_list(base: &[String], range: Range) -> [CompletionItem; 2] {
    let namespace = base.join("::");
    [
        CompletionItem {
            label: "*".to_string(),
            kind: Some(CompletionItemKind::OPERATOR),
            detail: Some(format!("everything `{namespace}` declares")),
            sort_text: Some("~*".to_string()),
            text_edit: Some(edit(range, "*")),
            ..Default::default()
        },
        CompletionItem {
            label: "{…}".to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some(format!("several items from `{namespace}`")),
            sort_text: Some("~{".to_string()),
            filter_text: Some("{".to_string()),
            text_edit: Some(edit(range, "{$0}")),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            command: Some(trigger_suggest()),
            ..Default::default()
        },
    ]
}

/// The namespace `segments` name, from the file at `uri`, the way the build
/// resolves a `use` path: `parent::common` is the sibling `common`.
fn resolve(segments: &[String], legacy: bool, uri: &Url) -> Option<Vec<String>> {
    if legacy {
        return Some(segments.to_vec()); // `import` predates relative prefixes
    }
    let path = UsePath::Absolute(ScopedIdentifier { text: format!("{}::_", segments.join("::")) });
    let resolver = ImportResolver::new(vec![], HashMap::new(), None);
    let mut namespace = resolver.resolve_namespace(&path, &imports::namespace_of(uri)).ok()?.absolute_namespace;
    namespace.pop();
    Some(namespace)
}

fn is_relative_prefix(segment: &str) -> bool {
    vocabulary::keyword(segment).is_some_and(|k| k.kind == KeywordKind::PathPrefix)
}

/// A path segment or a declaration's name. `continues`: the path must go on,
/// so `::` is inserted too and the next suggestions shown right away.
fn segment_item(label: &str, detail: String, kind: CompletionItemKind, continues: bool, range: Range) -> CompletionItem {
    let new_text = match continues {
        true => format!("{label}::"),
        false => label.to_string(),
    };
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: Some(detail),
        text_edit: Some(edit(range, &new_text)),
        command: continues.then(trigger_suggest),
        ..Default::default()
    }
}

fn sorted(mut item: CompletionItem, group: char) -> CompletionItem {
    item.sort_text = Some(format!("{group}{}", item.label));
    item
}

/// Replaces the segment being typed, whatever the client counts as a word.
fn edit(range: Range, new_text: &str) -> CompletionTextEdit {
    CompletionTextEdit::Edit(TextEdit { range, new_text: new_text.to_string() })
}

fn trigger_suggest() -> Command {
    Command { title: "Suggest".to_string(), command: "editor.action.triggerSuggest".to_string(), arguments: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::source::SourceFile;
    use crate::handlers::completion::get_completions_with_project;
    use crate::util::offset_to_position;

    fn path(segments: &[&str], listed: Option<&[&str]>, partial_start: usize) -> Option<UseContext> {
        Some(UseContext::Path(UsePrefix {
            legacy: false,
            segments: segments.iter().map(|s| s.to_string()).collect(),
            listed: listed.map(|l| l.iter().map(|s| s.to_string()).collect()),
            partial_start,
        }))
    }

    fn at_end(source: &str) -> Option<UseContext> {
        context_at(source, source.len())
    }

    #[test]
    fn reads_the_path_typed_so_far() {
        assert_eq!(at_end("use "), path(&[], None, 4));
        assert_eq!(at_end("use sh"), path(&[], None, 4));
        assert_eq!(at_end("use shared::"), path(&["shared"], None, 12));
        assert_eq!(at_end("use shared::models::Th"), path(&["shared", "models"], None, 20));
        assert_eq!(at_end("struct A {}\n\nuse parent::c"), path(&["parent"], None, 25));
    }

    #[test]
    fn reads_a_brace_list_on_one_line_or_several() {
        assert_eq!(at_end("use a::b::{X, Y"), path(&["a", "b"], Some(&["X"]), 14));
        assert_eq!(at_end("use a::b::{"), path(&["a", "b"], Some(&[]), 11));
        let source = "use a::b::{ // pick\n    X,\n    Y, // why\n    Z";
        assert_eq!(at_end(source), path(&["a", "b"], Some(&["X", "Y"]), source.len() - 1));
    }

    #[test]
    fn nothing_to_offer_after_the_path_or_mid_separator() {
        for source in ["use a::B ", "use a::B as ", "use a::B as Al", "use a:", "use a::{B} ", "use a::{B C", "use {"] {
            assert_eq!(at_end(source), Some(UseContext::Nothing), "{source:?}");
        }
    }

    #[test]
    fn not_a_use_statement() {
        for source in [
            "use",
            "struct A {\n    na",
            "use a::B\n\nst",
            "use a::{\n    B\n}\nst",
            "struct S {\n    user: u32\n    use",
            "protocol P {\n    function f",
        ] {
            assert_eq!(at_end(source), None, "{source:?}");
        }
    }

    #[test]
    fn the_legacy_import_takes_a_plain_path() {
        let Some(UseContext::Path(prefix)) = at_end("import types::") else { panic!() };
        assert!(prefix.legacy);
        assert_eq!(at_end("import types::{"), Some(UseContext::Nothing));
    }

    const MANIFEST: &str = "congregation app\nspecification_version = 1\n\ndependencies = {\n    \
        shared = {\n        path = \"../shared\"\n    }\n    \
        net = {\n        version = \"1.0.0\"\n        uri = \"https://example.test/net\"\n        commit = \"abc\"\n    }\n}\n";

    /// A package with `types`, `api/common`, `api/v1/user`, a fetched `shared`
    /// dependency and an unfetched `net` one, seen from `api/b.ids`.
    fn package() -> Vec<SourceFile> {
        let file = |path: &str| Url::parse(&format!("file:///pkg/{path}")).unwrap();
        vec![
            SourceFile::local(file("config.idp"), MANIFEST.to_string()),
            SourceFile::local(
                file("src/types.ids"),
                "struct User {\n    id: u64\n}\n\nenum Kind {\n    A\n}\n\nerror NotFound {\n    message = \"gone\"\n}\n"
                    .to_string(),
            ),
            SourceFile::local(
                file("src/api/common.ids"),
                "struct Error {\n    code: u32\n}\n\nstruct Other {\n    code: u32\n}\n".to_string(),
            ),
            SourceFile::local(file("src/api/v1/user.ids"), "struct Profile {\n    id: u64\n}\n".to_string()),
            SourceFile::of_dependency(
                Url::parse("file:///shared/src/models.ids").unwrap(),
                "struct Thing {\n    id: u64\n}\n\nstruct Gadget {\n    id: u64\n}\n".to_string(),
                "shared",
                vec!["shared".to_string(), "models".to_string()],
            ),
        ]
    }

    /// `(label, detail, inserted text)` for completions at the end of `source`
    /// in `pkg/src/api/b.ids`, in the order the client would show them.
    fn complete(source: &str) -> Vec<(String, String, String)> {
        let uri = Url::parse("file:///pkg/src/api/b.ids").unwrap();
        let mut items = get_completions_with_project(source, &uri, offset_to_position(source, source.len()), &package());
        items.sort_by(|a, b| a.sort_text.as_ref().unwrap_or(&a.label).cmp(b.sort_text.as_ref().unwrap_or(&b.label)));
        items
            .into_iter()
            .map(|c| {
                let Some(CompletionTextEdit::Edit(edit)) = c.text_edit else { panic!("no edit on {}", c.label) };
                (c.label, c.detail.unwrap_or_default(), edit.new_text)
            })
            .collect()
    }

    fn labels(items: &[(String, String, String)]) -> Vec<&str> {
        items.iter().map(|(label, _, _)| label.as_str()).collect()
    }

    #[test]
    fn a_path_starts_with_the_packages_namespaces_its_dependencies_and_the_prefixes() {
        let items = complete("use ");
        assert_eq!(labels(&items), ["api", "types", "net", "shared", "package", "parent", "self", "std"]);

        let find = |label: &str| items.iter().find(|(l, _, _)| l == label).unwrap();
        assert_eq!(find("api"), &("api".into(), "`api/`".into(), "api::".into()), "a directory goes on");
        assert_eq!(find("types"), &("types".into(), "`types.ids`".into(), "types".into()), "a schema can end a path");
        assert_eq!(find("shared").1, "dependency, path `../shared`");
        assert_eq!(find("net").1, "dependency, git — not fetched yet");
        assert_eq!(find("parent").1, "In a `use` path, one namespace level up.");
    }

    #[test]
    fn a_namespace_offers_whats_under_it() {
        let items = complete("use api::");
        assert_eq!(
            items,
            [
                ("common".into(), "`common.ids`".into(), "common".into()),
                ("v1".into(), "`v1/`".into(), "v1::".into()),
            ]
        );
    }

    #[test]
    fn a_schema_offers_its_declarations_then_glob_and_list() {
        let items = complete("use types::");
        assert_eq!(labels(&items), ["Kind", "NotFound", "User", "*", "{…}"]);
        assert_eq!(items[1].1, "error", "errors are importable too");
        assert_eq!(items[4].2, "{$0}");
    }

    #[test]
    fn relative_paths_resolve_like_the_build_and_stay_in_the_package() {
        // From `api/b.ids`, `parent::` is `api`.
        assert_eq!(labels(&complete("use parent::")), ["common", "v1"], "no `*` after a bare prefix: it wouldn't parse");
        assert_eq!(labels(&complete("use parent::common::")), ["Error", "Other", "*", "{…}"]);
        assert_eq!(labels(&complete("use package::")), ["api", "types"], "not the dependencies");
        assert_eq!(labels(&complete("use self::")), Vec::<&str>::new(), "`api::b` has nothing under it");
    }

    #[test]
    fn a_dependency_offers_its_schemas() {
        assert_eq!(complete("use shared::"), [("models".into(), "`models.ids` (dependency `shared`)".into(), "models".into())]);
        assert_eq!(labels(&complete("use shared::models::")), ["Gadget", "Thing", "*", "{…}"]);
        assert!(complete("use net::").is_empty(), "not fetched: nothing to list");
    }

    #[test]
    fn a_brace_list_offers_what_isnt_listed_yet() {
        assert_eq!(labels(&complete("use shared::models::{Thing, ")), ["Gadget"]);
        assert_eq!(labels(&complete("use parent::common::{\n    Other,\n    E")), ["Error"]);
    }

    #[test]
    fn an_unknown_path_offers_nothing() {
        assert!(complete("use typse::").is_empty());
        assert!(complete("use parent::parent::parent::").is_empty(), "above the package root");
    }

    #[test]
    fn the_typed_segment_is_what_gets_replaced() {
        let source = "use shared::mo";
        let uri = Url::parse("file:///pkg/src/api/b.ids").unwrap();
        let items = get_completions_with_project(source, &uri, offset_to_position(source, source.len()), &package());
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else { panic!() };
        assert_eq!((edit.range.start.character, edit.range.end.character), (12, 14));
        assert_eq!(items[0].command.as_ref().map(|c| c.command.as_str()), None, "`models` can end the path");
    }

    #[test]
    fn std_completes_like_a_dependency() {
        let uri = Url::parse("file:///pkg/src/api/b.ids").unwrap();
        let mut files = package();
        files.extend(crate::analysis::stdlib::files(&crate::analysis::stdlib::virtual_root()));
        let at = |source: &str| {
            let mut items = get_completions_with_project(source, &uri, offset_to_position(source, source.len()), &files);
            items.sort_by(|a, b| a.sort_text.as_ref().unwrap_or(&a.label).cmp(b.sort_text.as_ref().unwrap_or(&b.label)));
            items.into_iter().map(|c| (c.label, c.detail.unwrap_or_default())).collect::<Vec<_>>()
        };

        assert!(at("use ").contains(&("std".to_string(), "the standard library".to_string())));
        assert_eq!(
            at("use std::"),
            [("http".to_string(), "`http.ids` (std)".to_string()), ("validators".to_string(), "`validators.ids` (std)".to_string())]
        );
        let http: Vec<String> = at("use std::http::").into_iter().map(|(label, _)| label).collect();
        assert_eq!(http, ["HttpMethod", "Request", "Response", "*", "{…}"]);
    }

    #[test]
    fn the_legacy_import_offers_plain_paths_only() {
        assert_eq!(labels(&complete("import ")), ["api", "types", "net", "shared"]);
        assert_eq!(labels(&complete("import types::")), ["Kind", "NotFound", "User"]);
    }
}
