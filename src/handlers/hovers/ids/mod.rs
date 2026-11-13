//! Schema hover handler - provides type information on hover

#[cfg(test)]
mod tests;

use crate::analysis::imports::{self, ProjectFile};
use crate::analysis::modules::{self, Module};
use crate::analysis::project::Project;
use crate::analysis::source::{self, ProjectSource};
use crate::analysis::symbols;
use crate::handlers::annotation_hover::AnnotationHover;
use crate::handlers::{code, hover, recognized_keys};
use crate::parser;
use crate::util::{byte_range_to_lsp_range, in_comment_or_string, position_to_offset, word_range_at};

use comline_core::schema::idl::annotations;
use comline_core::schema::idl::grammar::{
    ArraySize, Declaration, Document, Expression, Field, Setting, Settings, SizeValue, Struct,
    TypeAlias, Type,
};
use comline_core::schema::idl::module_docs::summary;
use comline_core::schema::idl::size::{self, Range, SizeLookup, SizeTarget, Sizes};
use comline_core::schema::idl::vocabulary;
use comline_core::encodings::{official, EncodingKind};
use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
use comline_core::schema::ir::compiler::Compile;
use comline_core::settings::catalog;
use comline_core::settings::effective::{
    effective_declaration_settings, effective_schema_settings, schema_settings,
};
use lsp_types::{Hover, HoverContents, MarkedString, Position, Url};


/// Get hover information at a position, considering only this file.
pub fn get_hover_info(source: &str, uri: &Url, position: Position) -> Option<Hover> {
    get_hover_info_with_project::<(Url, String)>(source, uri, position, &[])
}

/// Get hover information at a position, also searching `other_files` (every
/// other file in the project, as `(uri, source)` pairs) for a struct/enum/
/// protocol/const the hovered word might name if it isn't declared in this
/// file. Two-tier lookup: first, the active file's own `use`/`import`
/// declarations are resolved and checked for one that actually brings the
/// word into scope from a specific sibling (`crate::analysis::imports`) —
/// the correct, `use`-scoped answer. Failing that, a flat, project-wide,
/// first-match scan across every other file still runs as a fallback ("no
/// `use` resolves this" can mean not-imported, a sibling that doesn't parse
/// mid-edit, or a `use` being typed just as often as a real miss), with one
/// appended note when the fallback match is in a
/// sibling no `use` here actually reaches.
pub fn get_hover_info_with_project<S: ProjectSource>(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[S],
) -> Option<Hover> {
    // Convert position to byte offset
    let offset = position_to_offset(source, position)?;

    // Inside a `//` comment or a string literal, nothing here is Comline
    // syntax — not an annotation key, not a symbol reference — regardless
    // of what text happens to sit under the cursor (`//@settings = Test`
    // must not hover as the `@settings` annotation). Same guard
    // `completion.rs` and `modules::path_module_at` already use.
    if in_comment_or_string(source, offset) {
        return None;
    }

    // A segment of a `::` path (`std`, `validators` in `use std::validators::X`):
    // the module it names. Only needs the text, so it works while the rest of
    // the file doesn't parse.
    if let Some(hover) = module_hover(source, uri, offset, other_files) {
        return Some(hover);
    }

    let document = parser::parse_document(source)?;

    // Cursor is literally inside a `settings { ... }` block's own text
    // (named or unnamed) — checked directly by span, not through the
    // symbol table (which only tracks *named* settings blocks, since an
    // unnamed one has no name to be a symbol under — see `symbols.rs`).
    // Ahead of the word and `@` annotation lookups.
    // This only needs `other_files` directly (for the manifest's package-
    // level settings), not any parsed sibling `Document`.
    if let Some(settings) = settings_block_at(&document, offset) {
        return Some(create_settings_hover(
            settings,
            offset,
            &document,
            other_files,
        ));
    }

    // Find what's at this position
    let (word_start, word_end) = word_range_at(source, offset)?;
    let word = &source[word_start..word_end];

    // An annotation key (`@timeout_ms`) — checked first since `@`
    // immediately before the word is an unambiguous signal, independent of
    // whether the same text happens to also name a symbol or a field
    // elsewhere (a field literally named `timeout_ms` is a real,
    // if confusing, possibility).
    if source[..word_start].ends_with('@') {
        return Some(match annotations::lookup(word) {
            Some(info) => create_annotation_hover(info),
            None => create_unknown_annotation_hover(word),
        });
    }

    // Parse every sibling file once, up front, and keep them all alive for
    // the rest of this call — not just whichever one happens to match the
    // hovered word. A struct's fields can reference types declared in other
    // files, so size computation needs every file in scope, not just the
    // document the matched symbol itself lives in.
    // Each with the namespace it's seen under - a dependency's file can't
    // say by its path (see `analysis::source`). The package manifest isn't
    // a schema.
    let mut parsed: Vec<(Sibling, Origin)> = other_files
        .iter()
        .filter(|file| !source::is_manifest(file.uri()))
        .filter_map(|file| {
            let document = parser::parse_document(file.text())?;
            let namespace = file
                .namespace()
                .map(<[String]>::to_vec)
                .unwrap_or_else(|| imports::namespace_of(file.uri()));
            Some(((file.uri(), file.text(), document), namespace))
        })
        .collect();

    // Only the files this one's `use`s reach - and the files *their* `use`s
    // reach, since a struct's size follows its fields into wherever they are
    // imported from. A file nothing imports isn't in scope here, so nothing
    // in it is either: not for the hover, not for a size estimate.
    let own_namespace = imports::namespace_of(uri);
    let own_imports = imports::resolved_imports(&document, &own_namespace);
    let mut reached = vec![false; parsed.len()];
    let mut pending = vec![imports::resolved_imports(&document, &own_namespace)];
    while let Some(uses) = pending.pop() {
        for (i, ((_, _, sibling), namespace)) in parsed.iter().enumerate() {
            if !reached[i] && imports_reach(&uses, namespace) {
                reached[i] = true;
                pending.push(imports::resolved_imports(sibling, namespace));
            }
        }
    }
    let mut reached = reached.into_iter();
    parsed.retain(|_| reached.next().unwrap_or(false));

    // `others` feeds `HoverSizeLookup` too (below) — put the siblings this
    // file imports from directly first (stable, so relative order within
    // each group is otherwise unchanged), so a name collision with a file
    // only reached through another's `use` resolves to the nearer one.
    parsed.sort_by_key(|(_, namespace)| !imports_reach(&own_imports, namespace));
    let (other_docs, origins): (Vec<Sibling>, Vec<Origin>) = parsed.into_iter().unzip();

    let lookup = HoverSizeLookup {
        local: &document,
        others: &other_docs,
        formats: size_formats(&package_format(other_files)),
    };

    let symbol_table = symbols::build_symbol_table(&document, uri, source);

    // A symbol in this file. The cursor sitting on a declaration's own
    // name resolves by position; anything else is a reference, resolved in
    // whatever namespace its position allows.
    //
    // By name alone, a file declaring `struct Test` beside a
    // `settings Test` answered both with whichever the symbol table had
    // kept - so hovering the struct showed the settings block.
    let own = symbol_table
        .at_offset(offset)
        .or_else(|| symbol_table.get_in(word, symbols::namespace_at(source, offset)));
    if let Some(symbol) = own {
        return Some(create_symbol_hover(symbol, &document, &lookup));
    }

    let siblings: Vec<ProjectFile> = other_docs
        .iter()
        .zip(&origins)
        .map(|((u, s, _), namespace)| ProjectFile {
            uri: u,
            source: s,
            namespace: namespace.clone(),
        })
        .collect();

    if let Some(resolved) = imports::resolve_symbol(word, &own_imports, &siblings) {
        let sibling = resolved.file;
        if let Some((_, _, other_document)) =
            other_docs.iter().find(|(u, _, _)| **u == *sibling.uri)
        {
            let other_table =
                symbols::build_symbol_table(other_document, sibling.uri, sibling.source);
            // Look up `real_name`, not `word` — they differ when `word`
            // is an `as` alias, which `sibling`'s own symbol table never
            // heard of (see `ResolvedSymbol::real_name`'s doc).
            // A `use` only ever brings in a declaration - there is no
            // import that names a settings preset.
            if let Some(symbol) =
                other_table.get_in(&resolved.real_name, symbols::Namespace::Declaration)
            {
                return Some(create_symbol_hover(symbol, other_document, &lookup));
            }
        }
    }

    // Check if it's a type reference
    if let Some(type_info) = find_type_at_position(&document, word) {
        return Some(create_type_hover(word, type_info));
    }

    // Check if it's a field reference
    if let Some(field_info) = find_field_info(&document, word, offset, &lookup) {
        return Some(create_field_hover(&field_info));
    }

    None
}

/// Whether one of `uses` points into the file seen under `namespace`.
fn imports_reach(uses: &[imports::ResolvedUse], namespace: &[String]) -> bool {
    !namespace.is_empty()
        && uses
            .iter()
            .any(|u| u.resolved.absolute_namespace.starts_with(namespace))
}

/// A parsed sibling file: its URI, text and tree.
type Sibling<'a> = (&'a Url, &'a str, Document);

/// Where a sibling sits: the namespace it's seen under.
type Origin = Vec<String>;

/// Resolves a bare type name to its declaration across the active document
/// plus the files its `use`s reach: local document first, then `others` in
/// order — directly imported siblings first (see the call site that builds
/// `others`). A name nothing imports stays unresolved, and a type that
/// names it has no size. Backs [`size::range_of_type`] /
/// [`size::range_of_struct`] for the size block in [`create_symbol_hover`],
/// and carries the formats those sizes are shown in.
struct HoverSizeLookup<'a> {
    local: &'a Document,
    others: &'a [Sibling<'a>],
    formats: Vec<SizeFormat>,
}

impl HoverSizeLookup<'_> {
    /// The documents a name is searched in, in order: local, then `others`.
    fn documents(&self) -> impl Iterator<Item = &Document> {
        std::iter::once(self.local).chain(self.others.iter().map(|(_, _, doc)| doc))
    }

    /// Follow a type through any `type` alias chain it starts as, to the
    /// final non-alias type - same search order as `resolve`, for the same
    /// reason (a chain can cross files). A small depth cap guards against a
    /// cyclic alias that somehow reached hover without going through
    /// `core`'s own cycle check.
    fn resolve_alias_chain(&self, ty: &Type) -> Type {
        let mut current = ty.clone();
        for _ in 0..16 {
            let Type::Named(id) = &current else { break };
            let name = id.to_string();
            match self.documents().find_map(|doc| type_alias_named(doc, &name)) {
                Some(t) => current = t.target_type().clone(),
                None => break,
            }
        }
        current
    }
}

/// A format the hover shows sizes in, with its size rules.
struct SizeFormat {
    name: &'static str,
    rules: &'static Sizes,
}

/// The formats a size is shown in: the package's own first (its
/// `wire.format`, as `config.idp` writes it), then each other official
/// format that has size rules. A format of the package's own declaring has
/// none, so isn't shown.
fn size_formats(own: &str) -> Vec<SizeFormat> {
    let mut names: Vec<&'static str> = official::resolve(EncodingKind::Format, own)
        .into_iter()
        .collect();
    for name in official::names(EncodingKind::Format) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .filter_map(|name| {
            Some(SizeFormat {
                name,
                rules: size::rules(name)?,
            })
        })
        .collect()
}

/// The `wire.format` the package's `config.idp` states, else core's default.
fn package_format<S: ProjectSource>(other_files: &[S]) -> String {
    source::own_manifest(other_files)
        .map(|manifest| source::declared_wire(manifest.text()).format)
        .unwrap_or_else(|| annotations::DEFAULT_FORMAT.to_string())
}

impl SizeLookup for HoverSizeLookup<'_> {
    fn resolve(&self, bare_name: &str) -> Option<SizeTarget<'_>> {
        self.documents().find_map(|doc| {
            doc.0.iter().find_map(|decl| match &**decl {
                Declaration::Struct(s) if s.name() == bare_name => Some(SizeTarget::Struct(s)),
                Declaration::Enum(e) if e.name() == bare_name => Some(SizeTarget::Enum(e)),
                Declaration::TypeAlias(t) if t.name() == bare_name => {
                    Some(SizeTarget::Alias(t.target_type()))
                }
                _ => None,
            })
        })
    }
}

/// The `type` alias `doc` declares under `name`.
fn type_alias_named<'a>(doc: &'a Document, name: &str) -> Option<&'a TypeAlias> {
    doc.0.iter().find_map(|decl| match &**decl {
        Declaration::TypeAlias(t) if t.name() == name => Some(t),
        _ => None,
    })
}

/// No constant has a value here: a size that names one has none.
fn no_constants(_: &str) -> Option<u64> {
    None
}

/// What `range_of` makes of each of the lookup's formats, the formats it
/// can't size left out.
fn ranges_by_format(
    lookup: &HoverSizeLookup,
    range_of: impl Fn(&Sizes) -> Option<Range>,
) -> Vec<(&'static str, Range)> {
    lookup
        .formats
        .iter()
        .filter_map(|format| Some((format.name, range_of(format.rules)?)))
        .collect()
}

/// A range as bytes: `8 B`, `9–137 B`, `9 B–unbounded` where
/// nothing bounds it, `9 B, at most 137 B` where the maximum is only a bound.
fn render_range(range: Range) -> String {
    match range.max {
        Some(max) if range.upper_bound => format!("{} B, at most {max} B", range.min),
        Some(max) if max == range.min => format!("{max} B"),
        Some(max) => format!("{}\u{2013}{max} B", range.min),
        None => format!("{} B\u{2013}unbounded", range.min),
    }
}

/// One bullet per format: `- packed@1: 9–137 B`.
fn render_format_ranges(ranges: &[(&str, Range)]) -> String {
    ranges
        .iter()
        .map(|(format, range)| format!("- {format}: {}", render_range(*range)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The size block of a hover: `**Wire size**` and a line per format; `None`
/// where no format can size it.
fn render_size_block(ranges: &[(&str, Range)]) -> Option<String> {
    (!ranges.is_empty()).then(|| format!("**Wire size**\n\n{}", render_format_ranges(ranges)))
}

/// A struct's size block: its total in each format, then a per-field
/// breakdown in the first (the package's own), one field per line,
/// prefixed with its wire index - the position `comline-rust`'s positional
/// (array-form) MsgPack encoding actually keys on, and the number the
/// append-only field discipline (see Versioning rules) protects.
/// `- #0 name: 8 B` / `- #1 name: 1 B–unbounded`; a field with no size
/// in that format is listed without one.
fn render_struct_size_block(s: &Struct, lookup: &HoverSizeLookup) -> Option<String> {
    let totals = ranges_by_format(lookup, |rules| {
        size::range_of_struct(s, rules, lookup, &no_constants)
    });

    let fields = render_field_sizes(s, lookup);
    let parts: Vec<String> = render_size_block(&totals)
        .into_iter()
        .chain((!fields.is_empty()).then_some(fields))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn render_field_sizes(s: &Struct, lookup: &HoverSizeLookup) -> String {
    let own = lookup.formats.first();

    s.fields()
        .iter()
        .enumerate()
        .map(|(index, f)| {
            let size = own
                .and_then(|format| size::range_of_field(f, format.rules, lookup, &no_constants))
                .map(|range| format!(": {}", render_range(range)))
                .unwrap_or_default();
            format!("- #{index} {}{size}", f.name())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many entries of a module's contents a hover lists before "and N more".
const LISTED: usize = 20;

/// Hover on a segment of a `::` path - `std` and `validators` in
/// `use std::validators::StringBounds`, or in a qualified type name: the
/// module that segment names, with its docs (`//!`), the modules below it and
/// the types it declares. Relative prefixes (`parent`, `self`) show the
/// module they stand for.
///
/// `None` when the cursor isn't on such a segment, when it's in a comment or
/// string, and for a last segment that names a declaration (`StringBounds`):
/// that's the declaration's hover.
fn module_hover<S: ProjectSource>(
    source: &str,
    uri: &Url,
    offset: usize,
    other_files: &[S],
) -> Option<Hover> {
    let project = Project::with_active(uri, source, other_files);
    let (namespace, range) = modules::path_module_at(source, uri, offset, &project)?;

    let module = modules::module(&project, &namespace)?;
    Some(Hover {
        contents: HoverContents::Array(module_contents(&module)),
        range: Some(byte_range_to_lsp_range(source, range.0, range.1)),
    })
}

/// A module's hover: what it is, its docs, then what's in it.
fn module_contents(module: &Module) -> Vec<MarkedString> {
    let kind = if module.package { "package" } else { "module" };
    let mut contents = vec![MarkedString::from_language_code(
        "comline".to_string(),
        format!("{kind} {}", module.namespace.join("::")),
    )];

    if let Some(docs) = &module.docs {
        contents.push(MarkedString::from_markdown(docs.clone()));
    }

    let mut lists = Vec::new();

    if !module.modules.is_empty() {
        let entries: Vec<(String, Option<&str>)> = module
            .modules
            .iter()
            .map(|m| (format!("`{}`", m.name), m.docs.as_deref()))
            .collect();
        lists.push(entries_list("Modules", &entries));
    }

    if !module.declarations.is_empty() {
        let entries: Vec<(String, Option<&str>)> = module
            .declarations
            .iter()
            .map(|d| {
                (
                    format!("`{} {}`", d.kind.keyword(), d.name),
                    d.docs.as_deref(),
                )
            })
            .collect();
        lists.push(entries_list("Declares", &entries));
    }

    if !lists.is_empty() {
        contents.push(MarkedString::from_markdown(lists.join("\n\n")));
    }

    contents
}

/// `**Heading**` and a bullet per entry: its label, then the first line of
/// its docs. Long lists are cut.
fn entries_list(heading: &str, entries: &[(String, Option<&str>)]) -> String {
    let mut lines = vec![format!("**{heading}**"), String::new()];

    for (label, docs) in entries.iter().take(LISTED) {
        match docs.map(summary).filter(|s| !s.is_empty()) {
            Some(summary) => lines.push(format!("- {label} — {summary}")),
            None => lines.push(format!("- {label}")),
        }
    }

    if entries.len() > LISTED {
        lines.push(format!("- …and {} more", entries.len() - LISTED));
    }

    lines.join("\n")
}

/// Create hover for a symbol (struct, enum, protocol, const, ...), from its
/// declaration in `document` - found by span, so a `struct Test` beside a
/// `settings Test` can't answer for the other.
fn create_symbol_hover(
    symbol: &symbols::Symbol,
    document: &Document,
    lookup: &HoverSizeLookup,
) -> Hover {
    let name = &symbol.name;
    let declaration = document.0.iter().find(|d| d.span == symbol.decl_span);

    // Its signature, its doc-comment when present, (struct/enum/const only -
    // where "total size" is meaningful) a wire-size estimate block, and what
    // its children are called.
    let (signature, doc, size_block, children) = match declaration.map(|d| &**d) {
        Some(Declaration::Struct(s)) => (
            block(&format!("struct {name}"), field_lines(s.fields())),
            s.docstring(),
            render_struct_size_block(s, lookup),
            "fields",
        ),
        Some(Declaration::Enum(e)) => (
            block(
                &format!("enum {name}"),
                e.variants()
                    .iter()
                    .map(|v| format!("  {}", v.identifier().text))
                    .collect(),
            ),
            e.docstring(),
            render_size_block(&ranges_by_format(lookup, |rules| size::range_of_enum(e, rules))),
            "variants",
        ),
        Some(Declaration::Protocol(p)) => {
            let functions = p
                .functions()
                .iter()
                .map(|f| {
                    let args = f
                        .args()
                        .as_ref()
                        .map(|args| {
                            std::iter::once(args.first().arg_type())
                                .chain(args.rest().iter().map(|ca| ca.arg_type().arg_type()))
                                .map(format_type)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    let ret = f
                        .return_type()
                        .as_ref()
                        .map(|rt| format!(" -> {}", format_type(rt.return_type())))
                        .unwrap_or_default();
                    format!("  function {}({args}){ret}", f.name())
                })
                .collect();
            (block(&format!("protocol {name}"), functions), p.docstring(), None, "functions")
        }
        Some(Declaration::Const(c)) => (
            format!("const {}: {}", c.name(), format_type(c.type_def())),
            c.docstring(),
            render_size_block(&ranges_by_format(lookup, |rules| {
                size::range_of_type(&c.type_def, rules, lookup, &no_constants)
            })),
            "",
        ),
        Some(Declaration::TypeAlias(t)) => (
            // Its annotations above it, as written - an alias's
            // `@validators` run wherever it's used, so they belong in
            // what it is.
            format!(
                "{}type {name} = {}",
                t.annotations()
                    .iter()
                    .map(|a| match a.value() {
                        Some(value) => format!("@{} = {value}\n", a.key()),
                        None => format!("@{}\n", a.key()),
                    })
                    .collect::<String>(),
                format_type(t.target_type()),
            ),
            alias_doc(t, lookup),
            None,
            "",
        ),
        Some(Declaration::Error(e)) => (
            block(&format!("error {name}"), field_lines(e.fields())),
            e.docstring(),
            None,
            "fields",
        ),
        Some(Declaration::Validator(v)) => {
            let params: Vec<String> = v
                .properties()
                .iter()
                .map(|p| format!("{}: {}", p.name(), format_type(p.property_type())))
                .collect();
            (
                format!("validator {name}({})", params.join(", ")),
                v.docstring(),
                None,
                "parameters",
            )
        }
        Some(Declaration::Settings(s)) => (settings_signature(s), s.docstring(), None, "settings"),
        _ => (name.clone(), None, None, ""),
    };

    let mut contents = vec![code(signature)];
    contents.extend(doc.into_iter().chain(size_block).map(MarkedString::from_markdown));
    if !symbol.children.is_empty() && !children.is_empty() {
        contents.push(MarkedString::from_markdown(format!(
            "*{} {children}*",
            symbol.children.len()
        )));
    }
    hover(contents)
}

/// A type alias's doc-comment, then the type it fully resolves to - only
/// called out when that differs from the one-level target written in the
/// source, matching Rust-IDE hover-reveals-the-aliased-type behavior,
/// without a redundant second line for the common `type X = <primitive>`
/// case.
fn alias_doc(t: &TypeAlias, lookup: &HoverSizeLookup) -> Option<String> {
    let resolved = format_type(&lookup.resolve_alias_chain(t.target_type()));
    if resolved == format_type(t.target_type()) {
        return t.docstring();
    }
    let resolves_to = format!("*resolves to* `{resolved}`");
    Some(match t.docstring() {
        Some(d) => format!("{d}\n\n{resolves_to}"),
        None => resolves_to,
    })
}

/// `head {`, one line per entry, `}`.
fn block(head: &str, lines: Vec<String>) -> String {
    format!("{head} {{\n{}\n}}", lines.join("\n"))
}

/// A struct's or error's fields, one `  optional name: type` line each.
fn field_lines(fields: &[rust_sitter::Spanned<Field>]) -> Vec<String> {
    fields
        .iter()
        .map(|f| {
            let opt = if f.optional() { "optional " } else { "" };
            format!("  {opt}{}: {}", f.name(), format_type(f.field_type()))
        })
        .collect()
}

/// A settings block as written, named or not.
fn settings_signature(settings: &Settings) -> String {
    let head = match settings.name() {
        Some(name) => format!("settings {name}"),
        None => "settings".to_string(),
    };
    let entries = settings
        .entries()
        .iter()
        .map(|e| format!("  {} = {}", e.key(), e.value_string()))
        .collect();
    block(&head, entries)
}

/// Find the `settings { ... }` (or `settings Name { ... }`) declaration
/// whose own span contains `offset`, by position rather than by name — the
/// symbol table only tracks *named* blocks (see `symbols.rs`), so this is
/// the only way to reach an unnamed one at all.
fn settings_block_at(document: &Document, offset: usize) -> Option<&Settings> {
    document.0.iter().find_map(|decl| {
        let (start, end) = decl.span;
        if offset >= start && offset < end {
            if let Declaration::Settings(s) = &**decl {
                return Some(s);
            }
        }
        None
    })
}

/// Hover for a `settings` block the cursor is literally inside of —
/// dispatches to entry-level (cursor on one specific `key = value` line)
/// or block-level (cursor on the `settings` keyword, the block's name, or
/// anywhere else in the block that isn't one specific entry).
fn create_settings_hover<S: ProjectSource>(
    settings: &Settings,
    offset: usize,
    document: &Document,
    other_files: &[S],
) -> Hover {
    match settings
        .entries()
        .iter()
        .find(|e| offset >= e.span.0 && offset < e.span.1)
    {
        Some(entry) => create_settings_entry_hover(settings, entry, document, other_files),
        None => create_settings_block_hover(settings),
    }
}

/// Block-level: the same raw signature/entries rendering the symbol-table
/// path uses for named blocks ([`settings_signature`]) — also reachable for
/// unnamed ones here — plus a "what's recognized here" catalog listing.
fn create_settings_block_hover(settings: &Settings) -> Hover {
    let mut contents = vec![code(settings_signature(settings))];
    if let Some(doc) = settings.docstring() {
        contents.push(MarkedString::from_markdown(doc));
    }

    contents.push(recognized_keys());

    contents.push(MarkedString::from_markdown(format!(
        "*{} settings*",
        settings.entries().len()
    )));

    hover(contents)
}

/// Entry-level: the raw as-written `key = value` (unchanged content), a
/// catalog match for what this key actually controls (if it matches a
/// shape `settings::enforcement` recognizes), and the resolved effective
/// value after package+schema(+preset) layering — not just the raw one.
fn create_settings_entry_hover<S: ProjectSource>(
    settings: &Settings,
    entry: &rust_sitter::Spanned<Setting>,
    document: &Document,
    other_files: &[S],
) -> Hover {
    let key = entry.key();
    let mut contents = vec![code(format!("{} = {}", key, entry.value_string()))];

    let key_path = entry.key_path();
    let segments: Vec<&str> = key_path.iter().map(String::as_str).collect();
    if let Some(m) = catalog::match_path(&segments) {
        contents.push(MarkedString::from_markdown(
            catalog::doc_for(&m).description.to_string(),
        ));
    }

    let package_settings = source::own_manifest(other_files)
        .map(
            |f| match source::resolve_cross_package_settings(f.text(), other_files) {
                Some(Ok(dict)) => dict,
                Some(Err(_)) => source::base_settings(),
                None => source::declared_settings(f.text()),
            },
        )
        .unwrap_or_else(source::base_settings);
    let schema_units = IncrementalInterpreter::from_declarations(document.0.clone());
    let schema = schema_settings(&schema_units);
    let schema_effective = effective_schema_settings(&package_settings, &schema);
    let effective =
        effective_declaration_settings(&schema_effective, &schema, settings.name().as_deref());

    // `{value}`, not `{value:?}`: the latter printed `Bool(false)` where
    // the file said `false`.
    let effective_line = match effective.get_path(&key) {
        Some(value) => format!("effective: `{value}`"),
        None => "effective: not set — allowed by default".to_string(),
    };
    let effective_line = match settings.name() {
        Some(name) => {
            format!("{effective_line} (applies to declarations using `@settings = {name}`)")
        }
        None => effective_line,
    };
    contents.push(MarkedString::from_markdown(effective_line));

    hover(contents)
}

/// Create hover for a type reference
fn create_type_hover(type_name: &str, type_kind: &str) -> Hover {
    hover(vec![
        code(type_name.to_string()),
        MarkedString::from_markdown(format!("*{type_kind}*")),
    ])
}

/// Hover for a known `@key` — its own name, description, default when
/// absent, expected value shape, and what actually reads it (or that
/// nothing does yet). Sourced from [`annotations::KNOWN_ANNOTATIONS`], the
/// same table `completion` reads for the key's suggestion, so the two
/// can't describe one key two different ways.
fn create_annotation_hover(info: &annotations::AnnotationInfo) -> Hover {
    let hover = AnnotationHover::new(info.key)
        .description(info.description)
        .default(info.default)
        .value(annotations::describe_value(info));

    if info.consumed_by.is_empty() {
        hover.warning("not consumed anywhere yet — decided, advisory metadata only")
    } else {
        hover.note(format!("consumed by: {}", info.consumed_by.join(", ")))
    }
    .build()
}

/// Hover for an `@key` this server doesn't have a description for.
/// `@key=value` is an open namespace — an unrecognised key is still
/// perfectly valid, just not one this server knows the meaning of (yet).
fn create_unknown_annotation_hover(key: &str) -> Hover {
    hover(vec![
        code(format!("@{key}")),
        MarkedString::from_markdown(
            "Not a recognised annotation — still parses and freezes fine (`@key=value` is \
             an open namespace), but nothing this server knows about reads it."
                .to_string(),
        ),
    ])
}

/// Everything a field's own hover shows, resolved at the hovered offset.
struct FieldHoverInfo {
    container: String,
    index: usize,
    total: usize,
    name: String,
    type_text: String,
    optional: bool,
    default: Option<String>,
    docstring: Option<String>,
    annotations: Vec<String>,
    /// Its range in each format that can size it.
    size: Vec<(&'static str, Range)>,
}

/// Create hover for a field: signature, docstring (if any), then a detail
/// block with its wire index, size, and any other attributes it carries.
fn create_field_hover(info: &FieldHoverInfo) -> Hover {
    let mut contents = vec![];

    let opt = if info.optional { "optional " } else { "" };
    let default = info
        .default
        .as_ref()
        .map(|d| format!(" = {d}"))
        .unwrap_or_default();
    contents.push(code(format!("{opt}{}: {}{default}", info.name, info.type_text)));

    if let Some(doc) = &info.docstring {
        contents.push(MarkedString::from_markdown(doc.clone()));
    }

    let mut detail = vec![format!(
        "field **#{}** of `{}` ({} field{})",
        info.index,
        info.container,
        info.total,
        if info.total == 1 { "" } else { "s" }
    )];
    if !info.size.is_empty() {
        detail.push(format!("size:\n\n{}", render_format_ranges(&info.size)));
    }
    detail.push(format!(
        "optional: {}",
        if info.optional { "yes" } else { "no" }
    ));
    if !info.annotations.is_empty() {
        detail.push(format!("annotations: {}", info.annotations.join(", ")));
    }
    contents.push(MarkedString::from_markdown(detail.join("\n\n")));

    hover(contents)
}

/// A size as written: `N` (exactly N) or `max N` (at most N, a bound),
/// N being an integer or a constant's name.
fn format_size(size: &ArraySize) -> String {
    let value = |v: &SizeValue| match v {
        SizeValue::Integer(i) => i.value().to_string(),
        SizeValue::Constant(c) => c.text.clone(),
    };
    match size {
        ArraySize::Max(max) => format!("max {}", value(&max.value)),
        ArraySize::Exact(exact) => value(exact),
    }
}

/// Format a type for display. Primitive arms delegate to
/// `vocabulary::primitive_name_of`; the rest (`Named`/`Array`/`Union`/
/// `Unit`) stay here since `vocabulary` has no opinion on composite
/// shapes (the array arm renders a literal `[N]`, which it never would).
fn format_type(ty: &Type) -> String {
    if let Some(name) = vocabulary::primitive_name_of(ty) {
        return name.to_string();
    }
    match ty {
        Type::Named(name) => name.text.clone(),
        Type::Array(arr) => {
            if let Some(size) = &arr.size {
                format!("{}[{}]", format_type(arr.elem_type()), format_size(size))
            } else {
                format!("{}[]", format_type(arr.elem_type()))
            }
        }
        Type::Union(u) => u
            .members()
            .iter()
            .map(format_type)
            .collect::<Vec<_>>()
            .join(" | "),
        Type::Unit(_) => "()".to_string(),
        // Every primitive variant returned above already.
        _ => unreachable!("primitive_name_of covers every Type variant not matched here"),
    }
}

/// Find type at position
fn find_type_at_position(document: &Document, word: &str) -> Option<&'static str> {
    // Check if it's a primitive type — delegates to `vocabulary` so this
    // can't list a name the grammar doesn't actually have (it used to
    // hand-roll this match, independently of the one in `completion.rs`
    // that had the `i8` bug).
    match vocabulary::primitive(word) {
        Some(p) => Some(p.description),
        None => {
            // Check if it's a user-defined type
            for decl in &document.0 {
                match &**decl {
                    Declaration::Struct(s) if s.name() == word => return Some("struct"),
                    Declaration::Enum(e) if e.name() == word => return Some("enum"),
                    Declaration::Protocol(p) if p.name() == word => return Some("protocol"),
                    Declaration::TypeAlias(t) if t.name() == word => return Some("type alias"),
                    _ => {}
                }
            }
            None
        }
    }
}

/// Find the field at `offset`, if the hovered `word` names one. Matched by
/// span, not name alone — a struct's own name-based symbol hover and the
/// type-reference hover both run first, so this only ever sees a word that
/// didn't resolve as a declaration or type name; disambiguating by span
/// still matters because two different structs can each have a field with
/// the same name. Field size can resolve a type declared in another
/// project file, same as a struct's total size, so this takes the same
/// `lookup` rather than searching only `document`.
fn find_field_info(
    document: &Document,
    word: &str,
    offset: usize,
    lookup: &HoverSizeLookup,
) -> Option<FieldHoverInfo> {
    document.0.iter().find_map(|decl| match &**decl {
        Declaration::Struct(s) => field_info_in(s.name(), s.fields(), word, offset, lookup),
        Declaration::Error(e) => field_info_in(e.name(), e.fields(), word, offset, lookup),
        _ => None,
    })
}

/// Shared by `Struct` and `Error` — both carry a flat `Vec<Spanned<Field>>`
/// in the same shape.
fn field_info_in(
    container: String,
    fields: &[rust_sitter::Spanned<Field>],
    word: &str,
    offset: usize,
    lookup: &HoverSizeLookup,
) -> Option<FieldHoverInfo> {
    let total = fields.len();
    fields.iter().enumerate().find_map(|(index, spanned)| {
        let (start, end) = spanned.span;
        if offset < start || offset >= end || spanned.name() != word {
            return None;
        }
        Some(FieldHoverInfo {
            container: container.clone(),
            index,
            total,
            name: spanned.name(),
            type_text: format_type(spanned.field_type()),
            optional: spanned.optional(),
            default: spanned.default_value().map(format_expression),
            docstring: spanned.docstring(),
            annotations: spanned
                .annotations()
                .iter()
                .map(|a| match a.value() {
                    Some(v) => format!("@{}={}", a.key(), v),
                    None => format!("@{}", a.key()),
                })
                .collect(),
            // `optional` is the field's, so it counts for the absent value too.
            size: ranges_by_format(lookup, |rules| {
                size::range_of_field(spanned, rules, lookup, &no_constants)
            }),
        })
    })
}

/// Render a field default / annotation-value expression back to source-like
/// text — just enough for a hover line, not a general pretty-printer.
fn format_expression(e: &Expression) -> String {
    match e {
        Expression::Bool(b) => b.as_text().to_string(),
        Expression::Integer(i) => i.value.to_string(),
        Expression::String(s) => format!("\"{}\"", s.value),
        Expression::FString(f) => f.source(),
        Expression::Path(p) => p.text.clone(),
        Expression::Identifier(i) => i.text.clone(),
    }
}
