//! `comline.toml`: the consumer's code-generation config, checked the way
//! `comline generate` reads it.
//!
//! The file's shape comes from `comline_codegen::config` - its types, and the
//! JSON Schema derived from them - so every key, type and enum value here is
//! the CLI's own. The text is read with `toml_edit` for spans, walked against
//! that schema so that every problem is reported at once (serde stops at the
//! first), and then parsed with [`ComlineToml::parse`] as well: anything the
//! walk let through that the CLI would still refuse is reported too.
//!
//! On top of the shape, what only the package can say: the languages
//! `config.idp` declares, the versions this build's generators handle, the
//! encodings a visible package declares and the ones the package uses, and
//! whether a `generator` path is a file. Nothing here ever runs a generator.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use comline_codegen::config::{self, ComlineToml};
use comline_codegen::protocol::EncodingMapping;
use comline_core::encodings::{official, EncodingKind, EncodingMapping as Suggested};
use comline_core::package::config::ir::frozen::FrozenUnit as ConfigUnit;
use comline_core::schema::idl::annotations;
use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
use comline_core::schema::ir::compiler::Compile;
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Url};
use serde_json::{json, Value as Json};
use toml_edit::{ImDocument, Item, TableLike, Value};

use crate::analysis::generation;
use crate::analysis::source::{self, DeclaredLanguage, ProjectSource};
use crate::util::byte_range_to_lsp_range;

/// The text isn't TOML.
pub const SYNTAX: &str = "comline-toml::syntax";
/// A key the file doesn't have.
pub const UNKNOWN_KEY: &str = "comline-toml::unknown-key";
/// A value of the wrong type.
pub const WRONG_TYPE: &str = "comline-toml::wrong-type";
/// A value outside what the key accepts.
pub const BAD_VALUE: &str = "comline-toml::bad-value";
/// A key that moved out of `comline.toml`.
pub const MOVED_KEY: &str = "comline-toml::moved-key";
/// Anything else `comline generate` refuses in the file's shape.
pub const INVALID: &str = "comline-toml::invalid";
/// A language `config.idp` doesn't declare.
pub const UNDECLARED_LANGUAGE: &str = "comline-toml::undeclared-language";
/// A version no generator in this build handles.
pub const LANG_VERSION: &str = "comline-toml::lang-version";
/// An `encodings` key no visible package declares.
pub const UNDECLARED_ENCODING: &str = "comline-toml::undeclared-encoding";
/// An encoding the package uses that a target has no mapping for.
pub const UNMAPPED_ENCODING: &str = "comline-toml::unmapped-encoding";
/// A `generator` path that isn't a file.
pub const GENERATOR_PATH: &str = "comline-toml::generator-path";

/// The fix a diagnostic's `data` carries.
pub const FIX_REPLACE: &str = "replace";
pub const FIX_DECLARE_LANGUAGE: &str = "declare-language";

/// Whether `uri` names a `comline.toml`.
pub fn is_comline_toml(uri: &Url) -> bool {
    uri.path().rsplit('/').next() == Some(config::FILE_NAME)
}

/// The schema of `comline.toml`, derived from the CLI's own types.
pub fn schema() -> &'static Json {
    static SCHEMA: std::sync::OnceLock<Json> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(config::json_schema)
}

/// An encoding some package in view declares, and which one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownEncoding {
    pub name: String,
    pub kind: EncodingKind,
    /// `comline (official)`, `this package`, or a dependency's name.
    pub package: String,
    pub map: BTreeMap<String, Suggested>,
}

/// Name of the package the official encodings come from, as shown.
pub const OFFICIAL: &str = "comline (official)";
/// The package `comline.toml` sits in, as shown.
pub const THIS_PACKAGE: &str = "this package";

/// What `comline.toml` is checked against: the package next to it.
#[derive(Debug, Default, Clone)]
pub struct Context {
    /// Whether a `config.idp` was found. Without one, nothing about the
    /// package is known and only the file's shape is checked.
    pub has_manifest: bool,
    pub languages: Vec<DeclaredLanguage>,
    /// The package's own and its dependencies' encodings (the official ones
    /// are always added).
    pub encodings: Vec<KnownEncoding>,
    /// The encodings the package's schemas use, canonical. `None` when they
    /// can't all be read (a schema mid-edit): then nothing is called unmapped.
    pub used: Option<BTreeSet<String>>,
    /// `comline.toml`'s directory, for `generator` paths. `None`: unchecked.
    pub base_dir: Option<PathBuf>,
}

impl Context {
    /// The context from the package's files in view: its `config.idp`, its
    /// schemas, and its dependencies' manifests (as `Backend::package_view`
    /// gives them).
    pub fn from_files<S: ProjectSource>(files: &[S], base_dir: Option<PathBuf>) -> Self {
        let mut context = Context { base_dir, ..Default::default() };
        let mut wire_format = annotations::DEFAULT_FORMAT.to_string();
        let mut schemas = Vec::new();
        let mut complete = true;
        for file in files {
            let (uri, text) = (file.uri(), file.text());
            if source::is_manifest(uri) {
                let package = file.dependency().map(str::to_string).unwrap_or_else(|| THIS_PACKAGE.to_string());
                if file.dependency().is_none() {
                    context.has_manifest = true;
                    context.languages = source::declared_languages(text);
                    wire_format = source::declared_wire_format(text);
                }
                context.encodings.extend(source::declared_encoding_details(text).into_iter().map(|e| KnownEncoding {
                    name: e.name,
                    kind: e.kind,
                    package: package.clone(),
                    map: e.map,
                }));
                continue;
            }
            if file.dependency().is_some() || crate::analysis::stdlib::is_std(uri) || !uri.path().ends_with(".ids") {
                continue;
            }
            match crate::parser::parse(text).ok().and_then(|r| if r.errors.is_empty() { r.document } else { None }) {
                Some(document) => {
                    let units = IncrementalInterpreter::from_declarations(document.0.clone());
                    schemas.push((crate::analysis::imports::namespace_of(uri).join("::"), units));
                }
                None => complete = false,
            }
        }
        if complete {
            context.used = Some(comline_codegen::encodings::used(&schemas, &wire_format));
        }
        context
    }

    /// Every encoding in view, the official ones first.
    pub fn all_encodings(&self) -> Vec<KnownEncoding> {
        let mut all: Vec<KnownEncoding> = official::units()
            .iter()
            .filter_map(|unit| match unit {
                ConfigUnit::Encoding { name, kind, map, .. } => Some(KnownEncoding {
                    name: name.clone(),
                    kind: *kind,
                    package: OFFICIAL.to_string(),
                    map: map.clone(),
                }),
                _ => None,
            })
            .collect();
        all.extend(self.encodings.iter().cloned());
        all
    }

    /// The encoding `name` (as written in `comline.toml`) means, if any.
    pub fn encoding(&self, name: &str) -> Option<KnownEncoding> {
        let canonical = comline_codegen::encodings::canonical(name);
        self.all_encodings()
            .into_iter()
            .find(|e| e.name == name || comline_codegen::encodings::canonical(&e.name) == canonical)
    }

    fn declares(&self, language: &str) -> Option<&DeclaredLanguage> {
        self.languages.iter().find(|d| d.language.eq_ignore_ascii_case(language))
    }
}

/// One key (or array element) of the file, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Keys from the root; an array's elements share their key's path.
    pub path: Vec<String>,
    /// The `[[generate.target]]` block it's in, by index.
    pub target: Option<usize>,
    pub key: (usize, usize),
    pub value: Option<(usize, usize)>,
    /// The value, when it's a string.
    pub text: Option<String>,
}

/// A `[[generate.target]]` block.
#[derive(Debug, Clone, Default)]
struct Block {
    header: (usize, usize),
}

/// The file read: every key, and what's wrong with its shape.
pub struct Analysis {
    pub entries: Vec<Entry>,
    blocks: Vec<Block>,
    generate_header: Option<(usize, usize)>,
    problems: Vec<Problem>,
    parsed: bool,
}

struct Problem {
    span: (usize, usize),
    code: &'static str,
    severity: DiagnosticSeverity,
    message: String,
    data: Option<Json>,
}

impl Analysis {
    /// The entry whose key or value is at `offset`, innermost first.
    pub fn entry_at(&self, offset: usize) -> Option<&Entry> {
        let within = |s: (usize, usize)| s.0 <= offset && offset <= s.1;
        self.entries
            .iter()
            .filter(|e| within(e.key) || e.value.is_some_and(within))
            .min_by_key(|e| e.value.map_or(e.key.1 - e.key.0, |v| v.1.max(e.key.1) - e.key.0))
    }

    /// The string value of `key` in target block `target`.
    fn target_value(&self, target: usize, key: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.target == Some(target) && e.path.len() == 3 && e.path[2] == key)
    }
}

/// Read `text`, collecting every key and every shape problem.
pub fn analyse(text: &str) -> Analysis {
    let mut analysis =
        Analysis { entries: vec![], blocks: vec![], generate_header: None, problems: vec![], parsed: false };
    let document = match ImDocument::parse(text) {
        Ok(document) => document,
        Err(e) => {
            let span = e.span().map_or((0, 0), |r| (r.start, r.end.max(r.start)));
            analysis.problems.push(Problem {
                span,
                code: SYNTAX,
                severity: DiagnosticSeverity::ERROR,
                message: e.message().trim().to_string(),
                data: None,
            });
            return analysis;
        }
    };
    analysis.parsed = true;
    let root = document.as_table();
    let mut walk = Walk { analysis: &mut analysis, text };
    walk.table(root, root.span().map(range).unwrap_or((0, 0)), schema(), &[], None);
    analysis
}

fn range(r: std::ops::Range<usize>) -> (usize, usize) {
    (r.start, r.end)
}

/// The TOML value walked, whatever holds it.
enum Node<'a> {
    Table(&'a dyn TableLike),
    Array(Vec<(Node<'a>, (usize, usize))>),
    Str(&'a str),
    Int(i64),
    Float,
    Bool,
    Datetime,
}

impl Node<'_> {
    fn type_name(&self) -> &'static str {
        match self {
            Node::Table(_) => "object",
            Node::Array(_) => "array",
            Node::Str(_) => "string",
            Node::Int(_) => "integer",
            Node::Float => "number",
            Node::Bool => "boolean",
            Node::Datetime => "datetime",
        }
    }

    fn shown(&self) -> &'static str {
        match self {
            Node::Table(_) => "a table",
            Node::Array(_) => "an array",
            Node::Str(_) => "a string",
            Node::Int(_) => "an integer",
            Node::Float => "a float",
            Node::Bool => "a boolean",
            Node::Datetime => "a date-time",
        }
    }
}

fn shown_type(schema_type: &str) -> &'static str {
    match schema_type {
        "object" => "a table",
        "array" => "an array",
        "string" => "a string",
        "integer" => "an integer",
        "number" => "a number",
        "boolean" => "a boolean (`true` or `false`)",
        _ => "something else",
    }
}

fn node_of(item: &Item) -> Option<(Node<'_>, Option<(usize, usize)>)> {
    Some(match item {
        Item::None => return None,
        Item::Table(t) => (Node::Table(t), t.span().map(range)),
        Item::ArrayOfTables(a) => (
            Node::Array(
                a.iter()
                    .map(|t| (Node::Table(t), t.span().map(range).unwrap_or((0, 0))))
                    .collect(),
            ),
            a.span().map(range),
        ),
        Item::Value(v) => (value_node(v), v.span().map(range)),
    })
}

fn value_node(value: &Value) -> Node<'_> {
    match value {
        Value::String(s) => Node::Str(s.value()),
        Value::Integer(i) => Node::Int(*i.value()),
        Value::Float(_) => Node::Float,
        Value::Boolean(_) => Node::Bool,
        Value::Datetime(_) => Node::Datetime,
        Value::Array(a) => Node::Array(
            a.iter().map(|v| (value_node(v), v.span().map(range).unwrap_or((0, 0)))).collect(),
        ),
        Value::InlineTable(t) => Node::Table(t),
    }
}

/// `node` with `$ref`s and single-branch `allOf`s followed.
pub fn deref(mut node: &Json) -> &Json {
    loop {
        if let Some(reference) = node.get("$ref").and_then(Json::as_str) {
            let name = reference.rsplit('/').next().unwrap_or_default();
            node = &schema()["definitions"][name];
        } else if let Some([only]) = node.get("allOf").and_then(Json::as_array).map(Vec::as_slice) {
            node = only;
        } else {
            return node;
        }
    }
}

/// The schema of the value at `path` (keys from the root; `target` is an
/// array, so its element), and of its key's own entry, for its description.
pub fn schema_at(path: &[String]) -> Option<&'static Json> {
    let mut node = schema();
    for key in path {
        let here = deref(node);
        node = match here.get("properties").and_then(|p| p.get(key.as_str())) {
            Some(property) => property,
            None => match here.get("additionalProperties") {
                Some(additional) if additional.is_object() => additional,
                _ => return None,
            },
        };
        if let Some(items) = deref(node).get("items") {
            if key == "target" || key == "only" {
                // An array of tables reads as its element.
                if key == "target" {
                    node = items;
                }
            }
        }
    }
    Some(node)
}

/// The description of the key at `path`: its own, else its type's.
pub fn description(path: &[String]) -> Option<String> {
    let node = schema_at(path)?;
    node.get("description").or_else(|| deref(node).get("description")).and_then(Json::as_str).map(str::to_string)
}

/// The keys a table at `path` has, with their descriptions.
pub fn keys_at(path: &[String]) -> Vec<(String, String)> {
    let Some(node) = schema_at(path) else { return vec![] };
    let node = deref(node);
    let node = node.get("items").map(deref).unwrap_or(node);
    node.get("properties")
        .and_then(Json::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(k, v)| {
                    let doc = v.get("description").or_else(|| deref(v).get("description"));
                    (k.clone(), doc.and_then(Json::as_str).unwrap_or_default().to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The values the key at `path` takes, when it's an enum or a boolean.
pub fn values_at(path: &[String]) -> Vec<String> {
    let Some(node) = schema_at(path) else { return vec![] };
    let node = deref(node);
    if let Some(values) = node.get("enum").and_then(Json::as_array) {
        return values.iter().filter_map(Json::as_str).map(|v| format!("\"{v}\"")).collect();
    }
    if node.get("type").and_then(Json::as_str) == Some("boolean") {
        return vec!["true".into(), "false".into()];
    }
    vec![]
}

struct Walk<'a, 'b> {
    analysis: &'a mut Analysis,
    text: &'b str,
}

impl Walk<'_, '_> {
    fn problem(&mut self, span: (usize, usize), code: &'static str, message: String, data: Option<Json>) {
        self.analysis.problems.push(Problem { span, code, severity: DiagnosticSeverity::ERROR, message, data });
    }

    fn table(&mut self, table: &dyn TableLike, span: (usize, usize), node: &Json, path: &[String], target: Option<usize>) {
        let node = deref(node);
        let properties = node.get("properties").and_then(Json::as_object);
        let additional = node.get("additionalProperties").filter(|a| a.is_object());
        for (key, item) in table.iter() {
            let Some((key_ref, _)) = table.get_key_value(key) else { continue };
            let key_span = key_ref.span().map(range).unwrap_or(span);
            let mut here = path.to_vec();
            here.push(key.to_string());
            let child = properties.and_then(|p| p.get(key)).or(additional);
            let Some(child) = child else {
                self.unknown(key, key_span, &here, properties, item, target);
                continue;
            };
            let Some((value, value_span)) = node_of(item) else { continue };
            let text = match &value {
                Node::Str(s) => Some(s.to_string()),
                _ => None,
            };
            self.analysis.entries.push(Entry {
                path: here.clone(),
                target,
                key: key_span,
                value: value_span.filter(|_| !matches!(item, Item::Table(_) | Item::ArrayOfTables(_))),
                text,
            });
            if here == ["generate"] {
                self.analysis.generate_header = Some(value_span.unwrap_or(key_span));
            }
            self.value(value, value_span.unwrap_or(key_span), child, &here, target);
        }
        if let Some(required) = node.get("required").and_then(Json::as_array) {
            for name in required.iter().filter_map(Json::as_str) {
                if !table.contains_key(name) {
                    self.problem(span, INVALID, format!("this needs `{name}`"), None);
                }
            }
        }
    }

    fn unknown(
        &mut self,
        key: &str,
        span: (usize, usize),
        path: &[String],
        properties: Option<&serde_json::Map<String, Json>>,
        item: &Item,
        target: Option<usize>,
    ) {
        let parent = &path[..path.len() - 1];
        if key == "default_framing" && (parent == ["generate"] || target.is_some()) {
            let written = item.as_str().unwrap_or("datagram@2");
            let framing = annotations::canonical_value("framing", written).unwrap_or(written);
            let span = item.span().map(|v| (span.0, v.end)).unwrap_or(span);
            self.problem(
                span,
                MOVED_KEY,
                format!(
                    "`default_framing` moved from {} to config.idp\n  help: drop it here, and declare the \
                     package's framing in config.idp:\n      wire = {{ framing = \"{framing}\" }}\n  Protocols \
                     with no `@framing` default to `datagram@2` without it.",
                    config::FILE_NAME
                ),
                Some(json!({ "fix": FIX_REPLACE, "candidates": [], "remove_line": true })),
            );
            return;
        }
        let names: Vec<&str> = properties.map(|p| p.keys().map(String::as_str).collect()).unwrap_or_default();
        let candidates = closest(key, &names);
        let place = if parent.is_empty() {
            "at the top level".to_string()
        } else if target.is_some() {
            "in `[[generate.target]]`".to_string()
        } else {
            format!("in `[{}]`", parent.join("."))
        };
        let mut message = format!("unknown key `{key}` {place}");
        match candidates.first() {
            Some(best) => message.push_str(&format!(" - did you mean `{best}`?")),
            None if !names.is_empty() => {
                message.push_str(&format!(" - expected one of {}", names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")))
            }
            None => {}
        }
        self.problem(span, UNKNOWN_KEY, message, Some(json!({ "fix": FIX_REPLACE, "candidates": candidates })));
    }

    fn value(&mut self, value: Node<'_>, span: (usize, usize), node: &Json, path: &[String], target: Option<usize>) {
        let node = deref(node);
        let key = path.last().map(String::as_str).unwrap_or_default();
        if let Some(branches) = node.get("anyOf").and_then(Json::as_array) {
            let matching = branches.iter().find(|b| deref(b).get("type").and_then(Json::as_str) == Some(value.type_name()));
            match matching {
                Some(branch) => self.value(value, span, branch, path, target),
                None => {
                    let expected: Vec<&str> = branches
                        .iter()
                        .filter_map(|b| deref(b).get("type").and_then(Json::as_str))
                        .map(shown_type)
                        .collect();
                    self.problem(
                        span,
                        WRONG_TYPE,
                        format!("`{key}` should be {}, not {}", expected.join(" or "), value.shown()),
                        None,
                    );
                }
            }
            return;
        }
        if let Some(expected) = node.get("type").and_then(Json::as_str) {
            if expected != value.type_name() {
                self.problem(span, WRONG_TYPE, format!("`{key}` should be {}, not {}", shown_type(expected), value.shown()), None);
                return;
            }
        }
        match value {
            Node::Str(s) => {
                if let Some(allowed) = node.get("enum").and_then(Json::as_array) {
                    let allowed: Vec<&str> = allowed.iter().filter_map(Json::as_str).collect();
                    if !allowed.contains(&s) {
                        let candidates: Vec<String> = closest(s, &allowed).into_iter().map(|c| format!("\"{c}\"")).collect();
                        self.problem(
                            span,
                            BAD_VALUE,
                            format!(
                                "`{key} = \"{s}\"` - expected one of {}",
                                allowed.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ")
                            ),
                            Some(json!({ "fix": FIX_REPLACE, "candidates": candidates })),
                        );
                    }
                }
            }
            Node::Int(i) => {
                if let Some(min) = node.get("minimum").and_then(Json::as_i64) {
                    if i < min {
                        let message = if key == "timeout" {
                            format!("`timeout = {i}` - it's in seconds, and must be at least {min}")
                        } else {
                            format!("`{key}` must be at least {min}")
                        };
                        self.problem(span, BAD_VALUE, message, None);
                    }
                }
            }
            Node::Table(table) => self.table(table, span, node, path, target),
            Node::Array(items) => {
                let item_schema = node.get("items").cloned().unwrap_or(Json::Null);
                for (index, (item, item_span)) in items.into_iter().enumerate() {
                    let target = if path == ["generate", "target"] {
                        self.analysis.blocks.push(Block { header: header_span(self.text, item_span) });
                        Some(index)
                    } else {
                        target
                    };
                    if let Node::Str(s) = &item {
                        self.analysis.entries.push(Entry {
                            path: path.to_vec(),
                            target,
                            key: item_span,
                            value: Some(item_span),
                            text: Some(s.to_string()),
                        });
                    }
                    self.value(item, item_span, &item_schema, path, target);
                }
            }
            Node::Float | Node::Bool | Node::Datetime => {}
        }
    }
}

/// A `[[generate.target]]` table's header line, from the table's span.
fn header_span(text: &str, span: (usize, usize)) -> (usize, usize) {
    let start = span.0.min(text.len());
    let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
    (start, end.min(span.1.max(start)).max(start))
}

/// Up to three of `names` closest to `written` by edit distance, within a
/// threshold that grows with the name's length; nearest first.
pub fn closest(written: &str, names: &[&str]) -> Vec<String> {
    let lower = written.to_lowercase();
    let threshold = match lower.chars().count() {
        0..=3 => 1,
        4..=8 => 2,
        _ => 3,
    };
    let mut scored: Vec<(usize, &str)> = names
        .iter()
        .map(|n| (distance(&lower, &n.to_lowercase()), *n))
        .filter(|(d, _)| *d <= threshold)
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, n)| n.to_string()).collect()
}

/// Optimal string alignment distance: insertions, deletions, substitutions
/// and adjacent transpositions each cost one.
pub fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// Every diagnostic for `text`, a `comline.toml`, in `context`.
pub fn diagnostics(text: &str, context: &Context) -> Vec<Diagnostic> {
    let mut analysis = analyse(text);
    if analysis.parsed {
        let shape_problems = analysis.problems.len();
        semantic(&mut analysis, context);
        // The walk covers the schema; serde is the CLI's own reader, so
        // anything it still refuses is reported too.
        if shape_problems == 0 {
            if let Err(e) = ComlineToml::parse(text) {
                let span = e.span().map_or((0, 0), range);
                analysis.problems.push(Problem {
                    span,
                    code: INVALID,
                    severity: DiagnosticSeverity::ERROR,
                    message: e.message().trim().to_string(),
                    data: None,
                });
            }
        }
    }
    analysis
        .problems
        .into_iter()
        .map(|p| Diagnostic {
            range: byte_range_to_lsp_range(text, p.span.0, p.span.1),
            severity: Some(p.severity),
            code: Some(NumberOrString::String(p.code.to_string())),
            source: Some("comline".to_string()),
            message: p.message,
            data: p.data,
            ..Default::default()
        })
        .collect()
}

fn semantic(analysis: &mut Analysis, context: &Context) {
    let mut problems = Vec::new();
    let mut push = |span, code, severity, message: String, data: Option<Json>| {
        problems.push(Problem { span, code, severity, message, data });
    };
    let error = DiagnosticSeverity::ERROR;
    let declared_names: Vec<&str> = context.languages.iter().map(|d| d.language.as_str()).collect();
    let check_declared = context.has_manifest && !context.languages.is_empty();

    let undeclared = |language: &str, span: (usize, usize), message: String, push: &mut dyn FnMut((usize, usize), &'static str, DiagnosticSeverity, String, Option<Json>)| {
        let candidates: Vec<String> = closest(language, &declared_names).into_iter().map(|c| format!("\"{c}\"")).collect();
        push(
            span,
            UNDECLARED_LANGUAGE,
            error,
            message,
            Some(json!({ "fix": FIX_DECLARE_LANGUAGE, "language": language, "candidates": candidates })),
        );
    };

    // `[generate] only`.
    for entry in analysis.entries.iter().filter(|e| e.path == ["generate", "only"] && e.text.is_some()) {
        let language = entry.text.as_deref().unwrap_or_default();
        if check_declared && context.declares(language).is_none() {
            undeclared(
                language,
                entry.key,
                format!("`[generate] only` names `{language}`, which config.idp does not declare"),
                &mut push,
            );
        }
    }

    // Each `[[generate.target]]`.
    for (index, block) in analysis.blocks.iter().enumerate() {
        let language = analysis.target_value(index, "language");
        let generator = analysis.target_value(index, "generator");
        let lang_version = analysis.target_value(index, "lang_version");
        let language_text = language.and_then(|e| e.text.clone()).unwrap_or_default();

        if language_text.is_empty() && generator.is_none() {
            push(block.header, INVALID, error, "a `[[generate.target]]` sets no `language`".to_string(), None);
        }
        if let (Some(entry), true) = (language, !language_text.is_empty()) {
            if check_declared && context.declares(&language_text).is_none() {
                undeclared(
                    &language_text,
                    entry.value.unwrap_or(entry.key),
                    format!(
                        "`[[generate.target]]` names `{language_text}`, which config.idp does not declare - add \
                         `{language_text}#<version> = {{}}` to `code_generation.languages`, or drop the block"
                    ),
                    &mut push,
                );
            }
        }

        // The version, for a built-in generator only: an external one says
        // what it handles itself, and is never run to ask.
        if generator.is_none() && !language_text.is_empty() {
            let declared = context.declares(&language_text);
            let raised = lang_version.and_then(|e| e.text.clone());
            let at = lang_version.or(language).map(|e| e.value.unwrap_or(e.key)).unwrap_or(block.header);
            if let (Some(raised), Some(declared)) = (&raised, declared) {
                use comline_codegen::versions::LangVersion;
                if let (Some(r), Some(d)) = (LangVersion::parse(raised), LangVersion::parse(&declared.version)) {
                    if r < d {
                        push(
                            at,
                            LANG_VERSION,
                            error,
                            format!(
                                "target `{language_text}` sets `lang_version = \"{raised}\"`, below config.idp's \
                                 `{language_text}#{}` - {} can raise a target's version, never lower it: the \
                                 package is only proven from `{}` on",
                                declared.version,
                                config::FILE_NAME,
                                declared.version
                            ),
                            None,
                        );
                    }
                }
            }
            let version = raised.or_else(|| declared.map(|d| d.version.clone()));
            if let Some(version) = version {
                if let Err(e) = generation::resolve(&language_text, &version) {
                    push(at, LANG_VERSION, error, e.to_string(), None);
                }
            }
        }

        // `timeout` limits an external generator.
        if let Some(timeout) = analysis.entries.iter().find(|e| e.target == Some(index) && e.path == ["generate", "target", "timeout"]) {
            if generator.is_none() {
                push(
                    timeout.key,
                    INVALID,
                    error,
                    format!(
                        "target `{language_text}`: `timeout` limits an external `generator`, and this target uses the \
                         built-in one"
                    ),
                    None,
                );
            }
        }

        // The `generator` path; only looked at, never run.
        if let (Some(entry), Some(dir)) = (generator, &context.base_dir) {
            let written = entry.text.clone().unwrap_or_default();
            let path = dir.join(&written);
            let message = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => None,
                Ok(_) => Some(format!("`generator = \"{written}\"` isn't a file ({})", path.display())),
                Err(_) => Some(format!(
                    "`generator = \"{written}\"` doesn't exist - it's relative to {}'s directory ({})",
                    config::FILE_NAME,
                    path.display()
                )),
            };
            if let Some(message) = message {
                push(entry.value.unwrap_or(entry.key), GENERATOR_PATH, error, message, None);
            }
        }
    }

    // `encodings` keys: declared somewhere in view.
    let known: Vec<String> = context.all_encodings().into_iter().map(|e| e.name).collect();
    let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
    for entry in analysis.entries.iter().filter(|e| is_encoding_key(&e.path)) {
        let name = entry.path.last().map(String::as_str).unwrap_or_default();
        if context.encoding(name).is_none() {
            let candidates: Vec<String> = closest(name, &known_refs).into_iter().map(|c| format!("\"{c}\"")).collect();
            push(
                entry.key,
                UNDECLARED_ENCODING,
                DiagnosticSeverity::WARNING,
                format!(
                    "no package in view declares the encoding `{name}` - not this package's config.idp, its \
                     dependencies', nor the official ones"
                ),
                Some(json!({ "fix": FIX_REPLACE, "candidates": candidates })),
            );
        }
    }

    // A used encoding with no mapping, target by target, as `comline
    // generate` resolves it.
    if let (true, Some(used)) = (context.has_manifest, &context.used) {
        let declared_units: Vec<ConfigUnit> = context
            .encodings
            .iter()
            .map(|e| ConfigUnit::Encoding {
                name: e.name.clone(),
                kind: e.kind,
                profile_hash: String::new(),
                map: e.map.clone(),
                establishes: Vec::new(),
                has_vectors: false,
            })
            .collect();
        let overrides = |target: Option<usize>| {
            analysis
                .entries
                .iter()
                .filter(|e| is_encoding_key(&e.path) && (e.target.is_none() || e.target == target))
                .map(|e| {
                    let name = e.path.last().cloned().unwrap_or_default();
                    let mapping = EncodingMapping { type_path: e.text.clone().unwrap_or_default(), package: None, version: None };
                    (comline_codegen::encodings::canonical(&name), mapping)
                })
                .collect::<BTreeMap<_, _>>()
        };
        let only: Vec<String> = analysis
            .entries
            .iter()
            .filter(|e| e.path == ["generate", "only"])
            .filter_map(|e| e.text.clone())
            .collect();
        // (language, block) for every target `comline generate` would build.
        let mut targets: Vec<(String, Option<usize>)> = Vec::new();
        let block_language =
            |i: usize| analysis.target_value(i, "language").and_then(|e| e.text.clone()).unwrap_or_default();
        if context.languages.is_empty() {
            targets.extend((0..analysis.blocks.len()).map(|i| (block_language(i), Some(i))));
        } else {
            for d in &context.languages {
                if !only.is_empty() && !only.iter().any(|l| l.eq_ignore_ascii_case(&d.language)) {
                    continue;
                }
                let blocks: Vec<usize> =
                    (0..analysis.blocks.len()).filter(|i| block_language(*i).eq_ignore_ascii_case(&d.language)).collect();
                if blocks.is_empty() {
                    targets.push((d.language.clone(), None));
                } else {
                    targets.extend(blocks.into_iter().map(|i| (d.language.clone(), Some(i))));
                }
            }
        }
        for (language, block) in targets.into_iter().filter(|(l, _)| !l.is_empty()) {
            let result = comline_codegen::encodings::resolve(
                &language,
                official::units(),
                &declared_units,
                &overrides(block),
                used,
            );
            if let Err(e) = result {
                let at = block
                    .map(|i| analysis.blocks[i].header)
                    .or(analysis.generate_header)
                    .unwrap_or((0, 0));
                for line in e.to_string().lines() {
                    let name = line.split('`').nth(1).unwrap_or_default().to_string();
                    let suggested = context.encoding(&name).and_then(|enc| enc.map.get(&language).cloned());
                    push(
                        at,
                        UNMAPPED_ENCODING,
                        error,
                        line.to_string(),
                        Some(json!({ "encoding": name, "language": language, "suggested": suggested.map(|s| s.ty) })),
                    );
                }
            }
        }
    }

    analysis.problems.extend(problems);
}

/// Whether `path` is an `encodings` key: `generate.encodings.<name>` or
/// `generate.target.encodings.<name>`.
pub fn is_encoding_key(path: &[String]) -> bool {
    matches!(path, [g, e, _] if g == "generate" && e == "encodings")
        || matches!(path, [g, t, e, _] if g == "generate" && t == "target" && e == "encodings")
}

/// How an encoding reads in hover: its kind, who declares it, and its
/// suggested mapping per language.
pub fn describe_encoding(encoding: &KnownEncoding) -> String {
    let kind = match encoding.kind {
        EncodingKind::Format => "format",
        EncodingKind::Framing => "framing",
        EncodingKind::Handshake => "handshake",
    };
    let mut out = format!("**`{}`** - a {kind}, declared by {}", encoding.name, encoding.package);
    if encoding.map.is_empty() {
        out.push_str("\n\nNo suggested mapping: map it here for each target.");
    } else {
        out.push_str("\n\nSuggested mapping:");
        for (language, mapping) in &encoding.map {
            out.push_str(&format!("\n- {language}: `{}`", mapping.ty));
            if let Some(package) = &mapping.package {
                out.push_str(&format!(" from `{package}`"));
                if let Some(version) = &mapping.version {
                    out.push_str(&format!(" `{version}`"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str, context: &Context) -> Vec<(String, String)> {
        diagnostics(text, context)
            .into_iter()
            .map(|d| match d.code {
                Some(NumberOrString::String(c)) => (c, d.message),
                _ => (String::new(), d.message),
            })
            .collect()
    }

    fn rust_package() -> Context {
        Context {
            has_manifest: true,
            languages: vec![DeclaredLanguage { language: "rust".into(), version: "1.85.0".into() }],
            encodings: vec![KnownEncoding {
                name: "com.acme.lines@1".into(),
                kind: EncodingKind::Framing,
                package: "acme".into(),
                map: BTreeMap::from([(
                    "rust".to_string(),
                    Suggested { ty: "acme::Lines".into(), package: Some("acme".into()), version: Some("^1".into()) },
                )]),
            }],
            used: Some(BTreeSet::new()),
            base_dir: None,
        }
    }

    #[test]
    fn a_valid_file_has_no_diagnostics() {
        let text = "[generate]\nout = \"gen\"\nmode = \"lib\"\nvalidate_on_decode = \"collect\"\n\n[[generate.target]]\nlanguage = \"rust\"\nlang_version = \"1.90.0\"\nencodings = { \"com.acme.lines@1\" = \"mine::Lines\" }\n";
        assert_eq!(codes(text, &rust_package()), vec![]);
    }

    #[test]
    fn a_syntax_error_has_its_span() {
        let d = diagnostics("[generate\nout = 1", &Context::default());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Some(NumberOrString::String(SYNTAX.into())));
        assert_eq!(d[0].range.start.line, 0);
    }

    #[test]
    fn every_shape_problem_is_reported_at_once() {
        let text = "colour = 1\n[generate]\nmdoe = \"code\"\nmode = \"libb\"\nout = 3\nvalidate_on_decode = \"frist\"\n\n[[generate.target]]\nlanguage = \"rust\"\ntimeout = 0\ngenerator = \"g.wasm\"\nflaten = true\n";
        let found = codes(text, &Context::default());
        let got: Vec<&str> = found.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(got.iter().filter(|c| **c == UNKNOWN_KEY).count(), 3, "{found:?}");
        assert_eq!(got.iter().filter(|c| **c == BAD_VALUE).count(), 3, "{found:?}");
        assert_eq!(got.iter().filter(|c| **c == WRONG_TYPE).count(), 1, "{found:?}");
        assert!(found.iter().any(|(_, m)| m.contains("did you mean `mode`")), "{found:?}");
        assert!(found.iter().any(|(_, m)| m.contains("did you mean `flatten`")), "{found:?}");
        assert!(found.iter().any(|(_, m)| m.contains("`timeout = 0` - it's in seconds")), "{found:?}");
        assert!(found.iter().any(|(_, m)| m == "`out` should be a string, not an integer"), "{found:?}");
    }

    #[test]
    fn a_typo_carries_its_replacements() {
        let d = diagnostics("[generate]\nmode = \"cod\"\n", &Context::default());
        assert_eq!(d[0].data.as_ref().unwrap()["candidates"], json!(["\"code\""]));
        assert_eq!((d[0].range.start.character, d[0].range.end.character), (7, 12));
        let d = diagnostics("[generate]\nlayuot = \"x\"\n", &Context::default());
        assert_eq!(d[0].data.as_ref().unwrap()["candidates"], json!(["layout"]));
        assert_eq!((d[0].range.start.character, d[0].range.end.character), (0, 6));
    }

    #[test]
    fn timeout_without_a_generator_is_refused() {
        let found = codes("[[generate.target]]\nlanguage = \"rust\"\ntimeout = 5\n", &rust_package());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].1.contains("`timeout` limits an external `generator`"));
    }

    #[test]
    fn default_framing_says_where_it_went() {
        let found = codes("[generate]\ndefault_framing = \"datagram\"\n", &Context::default());
        assert_eq!(found[0].0, MOVED_KEY);
        assert!(found[0].1.starts_with("`default_framing` moved from comline.toml to config.idp"), "{found:?}");
        assert!(found[0].1.contains("wire = { framing = \"datagram@1\" }") || found[0].1.contains("wire = { framing = \""), "{found:?}");
    }

    #[test]
    fn an_undeclared_language_is_an_error_with_its_fix_data() {
        let d = diagnostics("[generate]\nonly = [\"rsut\"]\n\n[[generate.target]]\nlanguage = \"typescript\"\n", &rust_package());
        let undeclared: Vec<&Diagnostic> =
            d.iter().filter(|d| d.code == Some(NumberOrString::String(UNDECLARED_LANGUAGE.into()))).collect();
        assert_eq!(undeclared.len(), 2, "{d:?}");
        assert_eq!(undeclared[0].data.as_ref().unwrap()["candidates"], json!(["\"rust\""]));
        assert_eq!(undeclared[1].data.as_ref().unwrap()["language"], json!("typescript"));
        assert!(undeclared[1].message.contains("which config.idp does not declare"));
    }

    #[test]
    fn a_version_no_builtin_handles_is_an_error_but_not_for_an_external_generator() {
        let found = codes("[[generate.target]]\nlanguage = \"rust\"\nlang_version = \"1.10\"\n", &rust_package());
        assert_eq!(found.iter().filter(|(c, _)| c == LANG_VERSION).count(), 2, "{found:?}");
        let found = codes("[[generate.target]]\nlanguage = \"rust\"\nlang_version = \"nightly\"\n", &rust_package());
        assert!(found.iter().any(|(c, m)| c == LANG_VERSION && m.contains("isn't a version")), "{found:?}");
        let found = codes(
            "[[generate.target]]\nlanguage = \"rust\"\nlang_version = \"nightly\"\ngenerator = \"g.wasm\"\n",
            &rust_package(),
        );
        assert!(!found.iter().any(|(c, _)| c == LANG_VERSION), "{found:?}");
    }

    #[test]
    fn an_undeclared_encoding_key_is_a_warning() {
        let d = diagnostics("[generate]\nencodings = { \"com.acme.lnes@1\" = \"x\", \"json-rpc\" = \"y\" }\n", &rust_package());
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(d[0].data.as_ref().unwrap()["candidates"], json!(["\"com.acme.lines@1\""]));
    }

    #[test]
    fn a_used_encoding_left_unmapped_is_an_error_per_target() {
        let mut context = rust_package();
        context.encodings[0].map.clear();
        context.used = Some(BTreeSet::from(["com.acme.lines@1".to_string()]));
        let found = codes("[generate]\nout = \"g\"\n", &context);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, UNMAPPED_ENCODING);
        assert!(found[0].1.starts_with("encoding `com.acme.lines@1` has no rust mapping"));
        let mapped = "[[generate.target]]\nlanguage = \"rust\"\nencodings = { \"com.acme.lines@1\" = \"mine::Lines\" }\n";
        assert_eq!(codes(mapped, &context), vec![]);
        // Unknown use (a schema mid-edit): nothing is called unmapped.
        context.used = None;
        assert_eq!(codes("[generate]\n", &context), vec![]);
    }

    #[test]
    fn a_generator_path_must_be_a_file() {
        let dir = std::env::temp_dir().join(format!("comline-toml-gen-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gens")).unwrap();
        std::fs::write(dir.join("g.wasm"), b"").unwrap();
        let mut context = rust_package();
        context.base_dir = Some(dir.clone());
        let block = |g: &str| format!("[[generate.target]]\nlanguage = \"rust\"\ngenerator = \"{g}\"\n");
        assert_eq!(codes(&block("g.wasm"), &context), vec![]);
        let found = codes(&block("missing.wasm"), &context);
        assert!(found.iter().any(|(c, m)| c == GENERATOR_PATH && m.contains("doesn't exist")), "{found:?}");
        let found = codes(&block("gens"), &context);
        assert!(found.iter().any(|(c, m)| c == GENERATOR_PATH && m.contains("isn't a file")), "{found:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn closest_has_a_threshold() {
        assert_eq!(closest("mdoe", &["mode", "out"]), vec!["mode"]);
        assert_eq!(closest("completely", &["mode", "out"]), Vec::<String>::new());
        assert_eq!(distance("flaten", "flatten"), 1);
    }

    #[test]
    fn the_context_reads_the_package() {
        use crate::analysis::source::SourceFile;
        let manifest = Url::parse("file:///p/config.idp").unwrap();
        let schema = Url::parse("file:///p/src/app.ids").unwrap();
        let dep = Url::parse("file:///d/config.idp").unwrap();
        let files = vec![
            SourceFile::local(manifest, "congregation p\ncode_generation = {\n    languages = {\n        rust#1.85.0 = {}\n    }\n}\n".into()),
            SourceFile::local(schema, "struct S {\n    id: u64\n}\n".into()),
            SourceFile::of_dependency(
                dep,
                "congregation d\nencodings = {\n    \"com.acme.x@1\" = { kind = \"format\" profile = \"x.json\" map = { rust = { type = \"acme::X\" package = \"acme\" } } }\n}\n".into(),
                "d",
                vec!["d".into()],
            ),
        ];
        let context = Context::from_files(&files, None);
        assert!(context.has_manifest);
        assert_eq!(context.languages[0].language, "rust");
        assert_eq!(context.encodings[0].package, "d");
        assert_eq!(context.encodings[0].map["rust"].ty, "acme::X");
        assert_eq!(context.used, Some(BTreeSet::from([annotations::DEFAULT_FORMAT.to_string()])));
    }
}
