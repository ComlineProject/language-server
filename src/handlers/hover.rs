// Hover handler - provides type information on hover

use crate::analysis::imports::{self, ProjectFile};
use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use comline_core::schema::idl::annotations;
use comline_core::schema::idl::grammar::{Declaration, Document, Expression, Field, Type};
use comline_core::schema::idl::size::{self, SizeLookup, SizeTarget, WireSize};
use comline_core::schema::idl::vocabulary;
use lsp_types::{Hover, HoverContents, MarkedString, Position, Url};

/// Get hover information at a position, considering only this file.
pub fn get_hover_info(source: &str, uri: &Url, position: Position) -> Option<Hover> {
    get_hover_info_with_project(source, uri, position, &[])
}

/// Get hover information at a position, also searching `other_files` (every
/// other file in the project, as `(uri, source)` pairs) for a struct/enum/
/// protocol/const the hovered word might name if it isn't declared in this
/// file. Two-tier lookup: first, the active file's own `use`/`import`
/// declarations are resolved and checked for one that actually brings the
/// word into scope from a specific sibling (`crate::analysis::imports`) —
/// the correct, `use`-scoped answer. Failing that, a flat, project-wide,
/// first-match scan across every other file still runs as a fallback (the
/// LSP's project view is only whichever files happen to be open — there's
/// no workspace scan on `initialize` — so "no `use` resolves this" can mean
/// not-imported, sibling-not-open, or mid-edit non-parsing just as often as
/// a real miss), with one appended note when the fallback match is in a
/// sibling no `use` here actually reaches.
pub fn get_hover_info_with_project(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[(Url, String)],
) -> Option<Hover> {
    // Convert position to byte offset
    let offset = position_to_offset(source, position)?;

    // Parse the document
    let parse_result = parser::parse(source).ok()?;
    let document = parse_result.document?;

    // Build symbol table
    let symbol_table = symbols::build_symbol_table(&document, uri, source);

    // Find what's at this position
    let word = get_word_at_offset(source, offset)?;

    // An annotation key (`@timeout_ms`) — checked first since `@`
    // immediately before the word is an unambiguous signal, independent of
    // whether the same text happens to also name a symbol or a field
    // elsewhere (a field literally named `timeout_ms` is a real,
    // if confusing, possibility).
    if is_annotation_key(source, offset) {
        return Some(match annotations::lookup(&word) {
            Some(info) => create_annotation_hover(info),
            None => create_unknown_annotation_hover(&word),
        });
    }

    // Parse every sibling file once, up front, and keep them all alive for
    // the rest of this call — not just whichever one happens to match the
    // hovered word. A struct's fields can reference types declared in *any*
    // other file, so size computation needs the whole set, not just the
    // document the matched symbol itself lives in.
    let mut other_docs: Vec<(&Url, &String, Document)> = other_files
        .iter()
        .filter_map(|(u, s)| parser::parse(s).ok()?.document.map(|d| (u, s, d)))
        .collect();

    // Check if a `use`/`import` here actually brings a symbol named `word`
    // into scope from one specific sibling.
    let own_namespace = imports::namespace_of(uri);
    let own_imports = imports::resolved_imports(&document, &own_namespace);

    // `others` feeds `HoverSizeLookup` too (below) — put `use`-resolved
    // siblings first (stable, so relative order within each group is
    // otherwise unchanged) so a name collision with an unimported sibling
    // resolves to the one actually in scope, while still falling back to
    // the unimported sibling's estimate over `Unknown`.
    other_docs.sort_by_key(|(u, _, _)| {
        !own_imports
            .iter()
            .any(|i| i.resolved.absolute_namespace.starts_with(&imports::namespace_of(u)))
    });

    let lookup = HoverSizeLookup { local: &document, others: &other_docs };

    // Check if it's a symbol in this file
    if let Some(symbol) = symbol_table.get(&word) {
        return Some(create_symbol_hover(symbol, &document, &lookup));
    }

    let siblings: Vec<ProjectFile> =
        other_docs.iter().map(|(u, s, _)| ProjectFile::new(u, s)).collect();

    if let Some(resolved) = imports::resolve_symbol(&word, &own_imports, &siblings) {
        let sibling = resolved.file;
        if let Some((_, _, other_document)) =
            other_docs.iter().find(|(u, _, _)| **u == *sibling.uri)
        {
            let other_table =
                symbols::build_symbol_table(other_document, sibling.uri, sibling.source);
            // Look up `real_name`, not `word` — they differ when `word`
            // is an `as` alias, which `sibling`'s own symbol table never
            // heard of (see `ResolvedSymbol::real_name`'s doc).
            if let Some(symbol) = other_table.get(&resolved.real_name) {
                return Some(create_symbol_hover(symbol, other_document, &lookup));
            }
        }
    }

    // Fallback: flat, project-wide, first-match scan — not `use`-scoped,
    // so append a note when the match isn't one any `use` here actually
    // reaches (never for a `std::` import: that's a different, legitimate
    // kind of not-locally-resolvable, not a "you forgot the use" case).
    for (other_uri, other_source, other_document) in &other_docs {
        let other_table = symbols::build_symbol_table(other_document, other_uri, other_source);
        if let Some(symbol) = other_table.get(&word) {
            let mut hover = create_symbol_hover(symbol, other_document, &lookup);
            if !resolves_via_std_import(&word, &own_imports) {
                append_note(
                    &mut hover,
                    format!(
                        "declared in `{}` — no `use` here brings it into scope",
                        file_label(other_uri)
                    ),
                );
            }
            return Some(hover);
        }
    }

    // Check if it's a type reference
    if let Some(type_info) = find_type_at_position(&document, &word) {
        return Some(create_type_hover(&word, type_info));
    }

    // Check if it's a field reference
    if let Some(field_info) = find_field_info(&document, &word, offset, &lookup) {
        return Some(create_field_hover(&field_info));
    }

    None
}

/// Whether the active file's own imports bring `word` into scope from
/// `std::` — the one case the fallback-match note (above) must stay quiet
/// for, since a `std::` symbol is never locally resolvable by design, not
/// because the author forgot a `use`. A simple last-segment check (not a
/// full `use_brings_into_scope` call): `std` siblings never exist in this
/// project, so there's nothing for `imports::resolve_symbol` to match —
/// this just answers "would it have, if `std` had a schema on disk."
fn resolves_via_std_import(word: &str, imports: &[imports::ResolvedUse]) -> bool {
    imports.iter().any(|u| {
        u.resolved.absolute_namespace.first().map(String::as_str) == Some("std")
            && u.resolved.absolute_namespace.last().map(String::as_str) == Some(word)
    })
}

/// The last path segment of a file's URI, for a short, readable hover note
/// (`types.ids`, not the full URI). `url.path()`, not `Url::to_file_path()`
/// — unavailable on `wasm32-unknown-unknown` (see `imports`'s module doc).
fn file_label(uri: &Url) -> String {
    std::path::Path::new(uri.path())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| uri.to_string())
}

/// Append one more line to an already-built hover.
fn append_note(hover: &mut Hover, note: String) {
    if let HoverContents::Array(contents) = &mut hover.contents {
        contents.push(MarkedString::from_markdown(note));
    }
}

/// Resolves a bare type name to its declaration across the active document
/// plus every other project file: local document first, then `others` in
/// order — `use`-resolved siblings sorted first (see the call site that
/// builds `others`), so a name collision with an unimported sibling still
/// resolves to the one actually in scope, but an unimported sibling's own
/// estimate still beats `Unknown` when nothing else matches. Backs
/// [`size::size_of_type`] / [`size::size_of_struct`] for the size block in
/// [`create_symbol_hover`].
struct HoverSizeLookup<'a> {
    local: &'a Document,
    others: &'a [(&'a Url, &'a String, Document)],
}

impl<'a> SizeLookup for HoverSizeLookup<'a> {
    fn resolve(&self, bare_name: &str) -> Option<SizeTarget<'_>> {
        if let Some(s) = find_struct_declaration(self.local, bare_name) {
            return Some(SizeTarget::Struct(s));
        }
        if let Some(e) = find_enum_declaration(self.local, bare_name) {
            return Some(SizeTarget::Enum(e));
        }
        if let Some(t) = find_type_alias_declaration(self.local, bare_name) {
            return Some(SizeTarget::Alias(t.target_type()));
        }
        for (_, _, doc) in self.others {
            if let Some(s) = find_struct_declaration(doc, bare_name) {
                return Some(SizeTarget::Struct(s));
            }
            if let Some(e) = find_enum_declaration(doc, bare_name) {
                return Some(SizeTarget::Enum(e));
            }
            if let Some(t) = find_type_alias_declaration(doc, bare_name) {
                return Some(SizeTarget::Alias(t.target_type()));
            }
        }
        None
    }
}

/// Follow a type through any `type` alias chain it starts as, to the final
/// non-alias type - same local-then-others search order as
/// `HoverSizeLookup::resolve`, for the same reason (a chain can cross
/// files). A small depth cap guards against a cyclic alias that somehow
/// reached hover without going through `core`'s own cycle check.
fn fully_resolve_alias_chain(
    ty: &Type,
    local: &Document,
    others: &[(&Url, &String, Document)],
) -> Type {
    let mut current = ty.clone();
    for _ in 0..16 {
        let Type::Named(id) = &current else { break };
        let name = id.to_string();
        let next = find_type_alias_declaration(local, &name)
            .or_else(|| others.iter().find_map(|(_, _, doc)| find_type_alias_declaration(doc, &name)))
            .map(|t| t.target_type().clone());
        match next {
            Some(t) => current = t,
            None => break,
        }
    }
    current
}

/// Render a computed size as the one-line summary hover shows for a struct
/// or enum: `wire size (fixed, raw-packed estimate): 16 bytes (128 bits)`,
/// `wire size: variable`, or `wire size: unknown` when a reference doesn't
/// resolve or a cycle was hit. Plain text, not markdown emphasis — this
/// joins the per-field breakdown into one multi-line prose block (see
/// `create_symbol_hover`), not a single-line caption like the "*N fields*"
/// detail line.
fn format_wire_size(size: WireSize) -> String {
    match size {
        WireSize::Fixed(bytes) => {
            format!("wire size (fixed, raw-packed estimate): {bytes} bytes ({} bits)", bytes * 8)
        }
        WireSize::Variable => "wire size: variable".to_string(),
        WireSize::Unknown => "wire size: unknown".to_string(),
    }
}

/// One-line rendering of a field's own size: `8 bytes` / `variable` /
/// `unknown` — shared between the struct's per-field breakdown and a single
/// field's own hover.
fn render_size_oneline(size: WireSize) -> String {
    match size {
        WireSize::Fixed(bytes) => format!("{bytes} bytes"),
        WireSize::Variable => "variable".to_string(),
        WireSize::Unknown => "unknown".to_string(),
    }
}

/// A field's declared size: `variable` the moment it's `optional` (absence
/// is itself variable-length), otherwise whatever its type resolves to.
fn field_size(f: &Field, lookup: &HoverSizeLookup) -> WireSize {
    if f.optional() {
        WireSize::Variable
    } else {
        size::size_of_type(f.field_type(), lookup)
    }
}

/// Per-field breakdown line for a struct's size block, one field per line,
/// prefixed with its wire index — the position `comline-rust`'s positional
/// (array-form) MsgPack encoding actually keys on, and the number the
/// append-only field discipline (see Versioning rules) protects.
/// `- #0 name: 8 bytes` / `- #1 name: variable`.
fn render_field_sizes(s: &comline_core::schema::idl::grammar::Struct, lookup: &HoverSizeLookup) -> String {
    s.fields()
        .iter()
        .enumerate()
        .map(|(index, f)| format!("- #{index} {}: {}", f.name(), render_size_oneline(field_size(f, lookup))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Create hover for a symbol (struct, enum, protocol, const)
fn create_symbol_hover(symbol: &symbols::Symbol, document: &Document, lookup: &HoverSizeLookup) -> Hover {
    use lsp_types::SymbolKind;

    let mut contents = vec![];

    // Add symbol signature, its doc-comment when present, and (struct/enum
    // only — where "total size" is meaningful) a wire-size estimate block.
    let (signature, doc, size_block) = match symbol.kind {
        SymbolKind::STRUCT => {
            // Find the struct to get its fields
            if let Some(s) = find_struct_declaration(document, &symbol.name) {
                let fields: Vec<String> = s.fields()
                    .iter()
                    .map(|f| {
                        let opt = if f.optional() { "optional " } else { "" };
                        format!("  {}{}: {}", opt, f.name(), format_type(f.field_type()))
                    })
                    .collect();

                let total = size::size_of_struct(s, lookup);
                let size_block = format!("{}\n{}", format_wire_size(total), render_field_sizes(s, lookup));

                (
                    format!("struct {} {{\n{}\n}}", symbol.name, fields.join("\n")),
                    s.docstring(),
                    Some(size_block),
                )
            } else {
                (format!("struct {}", symbol.name), None, None)
            }
        }
        SymbolKind::ENUM => {
            if let Some(e) = find_enum_declaration(document, &symbol.name) {
                let variants: Vec<String> = e.variants()
                    .iter()
                    .map(|v| format!("  {}", v.identifier().text))
                    .collect();

                let size_block = format_wire_size(size::size_of_enum(e));

                (
                    format!("enum {} {{\n{}\n}}", symbol.name, variants.join("\n")),
                    e.docstring(),
                    Some(size_block),
                )
            } else {
                (format!("enum {}", symbol.name), None, None)
            }
        }
        SymbolKind::INTERFACE => {
            if let Some(p) = find_protocol_declaration(document, &symbol.name) {
                let functions: Vec<String> = p.functions()
                    .iter()
                    .map(|f| {
                        let args = if let Some(args_list) = f.args() {
                            let mut arg_types = vec![format_type(args_list.first().arg_type())];
                            arg_types.extend(
                                args_list.rest().iter()
                                    .map(|ca| format_type(ca.arg_type().arg_type()))
                            );
                            arg_types.join(", ")
                        } else {
                            String::new()
                        };

                        let ret = if let Some(rt) = f.return_type() {
                            format!(" -> {}", format_type(rt.return_type()))
                        } else {
                            String::new()
                        };

                        format!("  function {}({}){}", f.name(), args, ret)
                    })
                    .collect();

                (format!("protocol {} {{\n{}\n}}", symbol.name, functions.join("\n")), p.docstring(), None)
            } else {
                (format!("protocol {}", symbol.name), None, None)
            }
        }
        SymbolKind::CONSTANT => {
            if let Some(c) = find_const_declaration(document, &symbol.name) {
                (format!("const {}: {}", c.name(), format_type(c.type_def())), c.docstring(), None)
            } else {
                (format!("const {}", symbol.name), None, None)
            }
        }
        SymbolKind::TYPE_PARAMETER => {
            if let Some(t) = find_type_alias_declaration(document, &symbol.name) {
                let target = t.target_type();
                let target_text = format_type(target);
                let signature = format!("type {} = {}", symbol.name, target_text);
                let resolved_text =
                    format_type(&fully_resolve_alias_chain(target, lookup.local, lookup.others));

                // Only call out the fully-resolved type when it differs from
                // the one-level target written in the source - matching
                // Rust-IDE hover-reveals-the-aliased-type behavior, without
                // a redundant second line for the common `type X =
                // <primitive>` case.
                let doc = if resolved_text != target_text {
                    let resolves_to = format!("*resolves to* `{}`", resolved_text);
                    Some(match t.docstring() {
                        Some(d) => format!("{}\n\n{}", d, resolves_to),
                        None => resolves_to,
                    })
                } else {
                    t.docstring()
                };
                (signature, doc, None)
            } else {
                (format!("type {}", symbol.name), None, None)
            }
        }
        _ => (symbol.name.clone(), None, None),
    };

    contents.push(MarkedString::from_language_code("comline".to_string(), signature));
    if let Some(doc) = doc {
        contents.push(MarkedString::from_markdown(doc));
    }
    if let Some(size_block) = size_block {
        contents.push(MarkedString::from_markdown(size_block));
    }

    // Add detail
    if !symbol.children.is_empty() {
        let detail = match symbol.kind {
            SymbolKind::STRUCT => format!("{} fields", symbol.children.len()),
            SymbolKind::ENUM => format!("{} variants", symbol.children.len()),
            SymbolKind::INTERFACE => format!("{} functions", symbol.children.len()),
            _ => String::new(),
        };
        if !detail.is_empty() {
            contents.push(MarkedString::from_markdown(format!("*{}*", detail)));
        }
    }
    
    Hover {
        contents: HoverContents::Array(contents),
        range: None,
    }
}

/// Create hover for a type reference
fn create_type_hover(type_name: &str, type_kind: &str) -> Hover {
    let contents = vec![
        MarkedString::from_language_code("comline".to_string(), type_name.to_string()),
        MarkedString::from_markdown(format!("*{}*", type_kind)),
    ];
    
    Hover {
        contents: HoverContents::Array(contents),
        range: None,
    }
}

/// Whether the word at `offset` is immediately preceded by `@` — i.e. this
/// is an annotation key (`@timeout_ms`), not a coincidental identifier
/// that happens to share its text with one (a field literally named
/// `timeout_ms`, say). Same start-boundary scan as
/// [`get_word_at_offset`], so it agrees with it on where the word begins.
fn is_annotation_key(source: &str, offset: usize) -> bool {
    let offset = offset.min(source.len());
    let start = source[..offset]
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    start > 0 && source.as_bytes().get(start - 1) == Some(&b'@')
}

/// Hover for a known `@key` — its own name, description, default when
/// absent, expected value shape, and what actually reads it (or that
/// nothing does yet). Sourced from [`annotations::KNOWN_ANNOTATIONS`], the
/// same table `completion` reads for the key's suggestion, so the two
/// can't describe one key two different ways.
fn create_annotation_hover(info: &annotations::AnnotationInfo) -> Hover {
    let consumed = if info.consumed_by.is_empty() {
        "**not consumed anywhere yet** — decided, advisory metadata only".to_string()
    } else {
        format!("consumed by: {}", info.consumed_by.join(", "))
    };
    let detail = [
        format!("default: {}", info.default),
        format!("value: {}", info.value),
        consumed,
    ]
    .join("\n\n");

    Hover {
        contents: HoverContents::Array(vec![
            MarkedString::from_language_code("comline".to_string(), format!("@{}", info.key)),
            MarkedString::from_markdown(info.description.to_string()),
            MarkedString::from_markdown(detail),
        ]),
        range: None,
    }
}

/// Hover for an `@key` this server doesn't have a description for.
/// `@key=value` is an open namespace — an unrecognised key is still
/// perfectly valid, just not one this server knows the meaning of (yet).
fn create_unknown_annotation_hover(key: &str) -> Hover {
    Hover {
        contents: HoverContents::Array(vec![
            MarkedString::from_language_code("comline".to_string(), format!("@{key}")),
            MarkedString::from_markdown(
                "Not a recognised annotation — still parses and freezes fine (`@key=value` is \
                 an open namespace), but nothing this server knows about reads it."
                    .to_string(),
            ),
        ]),
        range: None,
    }
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
    size: WireSize,
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
    let signature = format!("{opt}{}: {}{default}", info.name, info.type_text);
    contents.push(MarkedString::from_language_code("comline".to_string(), signature));

    if let Some(doc) = &info.docstring {
        contents.push(MarkedString::from_markdown(doc.clone()));
    }

    let mut detail = vec![
        format!("field **#{}** of `{}` ({} field{})", info.index, info.container, info.total, if info.total == 1 { "" } else { "s" }),
        format!("size: {}", render_size_oneline(info.size)),
    ];
    detail.push(format!("optional: {}", if info.optional { "yes" } else { "no" }));
    if !info.annotations.is_empty() {
        detail.push(format!("annotations: {}", info.annotations.join(", ")));
    }
    contents.push(MarkedString::from_markdown(detail.join("\n\n")));

    Hover {
        contents: HoverContents::Array(contents),
        range: None,
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
                format!("{}[{}]", format_type(arr.elem_type()), size.value)
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

/// Get word at byte offset
fn get_word_at_offset(source: &str, offset: usize) -> Option<String> {
    if offset >= source.len() {
        return None;
    }
    
    // Find word boundaries
    let start = source[..offset]
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    
    let end = source[offset..]
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| offset + i)
        .unwrap_or(source.len());
    
    Some(source[start..end].to_string())
}

/// Find type at position
fn find_type_at_position(document: &comline_core::schema::idl::grammar::Document, word: &str) -> Option<&'static str> {
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
    for decl in &document.0 {
        let found = match &**decl {
            Declaration::Struct(s) => field_info_in(s.name(), s.fields(), word, offset, lookup),
            Declaration::Error(e) => field_info_in(e.name(), e.fields(), word, offset, lookup),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
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
            size: field_size(spanned, lookup),
        })
    })
}

/// Render a field default / annotation-value expression back to source-like
/// text — just enough for a hover line, not a general pretty-printer.
fn format_expression(e: &Expression) -> String {
    match e {
        Expression::Integer(i) => i.value.to_string(),
        Expression::String(s) => format!("\"{}\"", s.value),
        Expression::FString(f) => f.source(),
        Expression::Path(p) => p.text.clone(),
        Expression::Identifier(i) => i.text.clone(),
    }
}

// Helper functions to find declarations
fn find_struct_declaration<'a>(document: &'a comline_core::schema::idl::grammar::Document, name: &str) -> Option<&'a comline_core::schema::idl::grammar::Struct> {
    for decl in &document.0 {
        if let Declaration::Struct(s) = &**decl {
            if s.name() == name {
                return Some(s);
            }
        }
    }
    None
}

fn find_enum_declaration<'a>(document: &'a comline_core::schema::idl::grammar::Document, name: &str) -> Option<&'a comline_core::schema::idl::grammar::Enum> {
    for decl in &document.0 {
        if let Declaration::Enum(e) = &**decl {
            if e.name() == name {
                return Some(e);
            }
        }
    }
    None
}

fn find_protocol_declaration<'a>(document: &'a comline_core::schema::idl::grammar::Document, name: &str) -> Option<&'a comline_core::schema::idl::grammar::Protocol> {
    for decl in &document.0 {
        if let Declaration::Protocol(p) = &**decl {
            if p.name() == name {
                return Some(p);
            }
        }
    }
    None
}

fn find_const_declaration<'a>(document: &'a comline_core::schema::idl::grammar::Document, name: &str) -> Option<&'a comline_core::schema::idl::grammar::Const> {
    for decl in &document.0 {
        if let Declaration::Const(c) = &**decl {
            if c.name() == name {
                return Some(c);
            }
        }
    }
    None
}

fn find_type_alias_declaration<'a>(document: &'a comline_core::schema::idl::grammar::Document, name: &str) -> Option<&'a comline_core::schema::idl::grammar::TypeAlias> {
    for decl in &document.0 {
        if let Declaration::TypeAlias(t) = &**decl {
            if t.name() == name {
                return Some(t);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_hover_on_struct() {
        let source = r#"
struct User {
    name: string
    age: i32
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "User" on line 1
        let position = Position::new(1, 8);
        
        let hover = get_hover_info(source, &uri, position);
        assert!(hover.is_some());

        let hover = hover.unwrap();
        if let HoverContents::Array(contents) = hover.contents {
            assert!(!contents.is_empty());
        }
    }

    #[test]
    fn test_hover_shows_fixed_struct_size() {
        let source = "struct Point {\n    x: u32\n    y: u32\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "Point" on line 0.
        let position = Position::new(0, 8);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("8 bytes"), "got: {text}");
        assert!(text.contains("64 bits"), "got: {text}");
    }

    #[test]
    fn test_hover_shows_variable_size_for_a_string_field() {
        let source = "struct Note {\n    body: string\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "Note" on line 0.
        let position = Position::new(0, 7);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("variable"), "got: {text}");
    }

    #[test]
    fn test_hover_shows_variable_size_for_an_optional_field() {
        let source = "struct Thing {\n    optional id: u64\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "Thing" on line 0.
        let position = Position::new(0, 8);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("variable"), "got: {text}");
    }

    #[test]
    fn test_struct_hover_shows_field_indices() {
        let source = "struct Greeting {\n    message: string\n    language: string\n    test: bool\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "Greeting" on line 0.
        let position = Position::new(0, 8);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("#0 message"), "got: {text}");
        assert!(text.contains("#1 language"), "got: {text}");
        assert!(text.contains("#2 test"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_field_name_shows_its_own_info() {
        let source = "struct Greeting {\n    message: string\n    language: string\n    test: bool\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "language" (the field name) on line 2.
        let position = Position::new(2, 6);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("language: string"), "got: {text}");
        assert!(text.contains("#1"), "got: {text}");
        assert!(text.contains("of `Greeting`"), "got: {text}");
        assert!(text.contains("3 fields"), "got: {text}");
        assert!(text.contains("optional: no"), "got: {text}");
        assert!(text.contains("variable"), "got: {text}"); // `string` is unbounded
    }

    #[test]
    fn hover_on_timeout_ms_annotation_shows_its_description() {
        let source = "protocol Thing {\n    @timeout_ms = 100\n    function foo();\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "timeout_ms" on line 1.
        let position = Position::new(1, 7);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@timeout_ms"), "got: {text}");
        assert!(text.contains("waits"), "got: {text}");
        assert!(text.contains("milliseconds"), "got: {text}");
        assert!(text.contains("comline-rust"), "got: {text}");
    }

    #[test]
    fn hover_on_framing_annotation_shows_its_default() {
        let source = "@framing = \"jsonrpc\"\nprotocol Thing {\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(0, 3);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@framing"), "got: {text}");
        assert!(text.contains("datagram"), "got: {text}");
    }

    #[test]
    fn hover_on_validators_annotation_shows_its_description() {
        let source = "struct X {\n    @validators = [Foo()]\n    name: str\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 7);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@validators"), "got: {text}");
        assert!(text.contains("validator"), "got: {text}");
    }

    #[test]
    fn hover_on_idempotent_annotation_says_not_consumed_yet() {
        let source = "protocol P {\n    @idempotent\n    function f();\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 8);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@idempotent"), "got: {text}");
        assert!(text.contains("not consumed anywhere yet"), "got: {text}");
    }

    #[test]
    fn hover_on_an_unrecognised_annotation_still_shows_something() {
        let source = "protocol P {\n    @custom_key = 1\n    function f();\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 8);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@custom_key"), "got: {text}");
        assert!(text.contains("open namespace"), "got: {text}");
    }

    #[test]
    fn hover_on_a_field_named_like_an_annotation_key_gets_field_hover_not_annotation_hover() {
        // No `@` prefix — "timeout_ms" here is an ordinary field name, not
        // the annotation. Disambiguated by `is_annotation_key`'s preceding-
        // `@` check, not by the word's text.
        let source = "struct X {\n    timeout_ms: u32\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 6);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("timeout_ms: u32"), "got: {text}");
        assert!(!text.contains("waits indefinitely"), "got: {text}");
    }

    #[test]
    fn test_hover_on_an_optional_field_shows_optional_and_default() {
        let source = "struct Thing {\n    optional id: u64 = 0\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "id" (the field name) on line 1.
        let position = Position::new(1, 14);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("optional id: u64 = 0"), "got: {text}");
        assert!(text.contains("optional: yes"), "got: {text}");
        // Optional is always variable at the wire, regardless of `u64`'s
        // own fixed size, since it needs a presence sentinel.
        assert!(text.contains("size: variable"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_field_shows_its_annotations() {
        let source = "struct Message {\n    @timeout_ms=1000\n    body: str\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "body" (the field name) on line 2.
        let position = Position::new(2, 6);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@timeout_ms=1000"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_field_in_an_error_shows_its_own_info() {
        let source = "error Rejected {\n    message = \"no\"\n    reason: str\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "reason" (the field name) on line 2.
        let position = Position::new(2, 6);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("reason: str"), "got: {text}");
        assert!(text.contains("of `Rejected`"), "got: {text}");
    }

    #[test]
    fn test_hover_on_enum() {
        let source = r#"
enum Status {
    Active
    Inactive
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let position = Position::new(1, 6);
        
        let hover = get_hover_info(source, &uri, position);
        assert!(hover.is_some());
    }
    
    #[test]
    fn test_hover_on_type() {
        let source = r#"
struct User {
    name: string
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "string" type
        let position = Position::new(2, 11);

        let hover = get_hover_info(source, &uri, position);
        assert!(hover.is_some());
    }

    fn hover_text(hover: Hover) -> String {
        match hover.contents {
            HoverContents::Array(parts) => parts
                .into_iter()
                .map(|p| match p {
                    MarkedString::String(s) => s,
                    MarkedString::LanguageString(ls) => ls.value,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            HoverContents::Scalar(MarkedString::String(s)) => s,
            HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
            HoverContents::Markup(m) => m.value,
        }
    }

    #[test]
    fn test_hover_on_struct_declared_in_another_file() {
        // `chat.ids` references `Message`, declared only in `types.ids`.
        let chat_source = r#"
protocol Chat {
    function send(text: string) -> Message;
}
"#;
        let types_source = "struct Message {\n    text: string\n    seq: u64\n}\n";

        let chat_uri = Url::parse("file:///chat.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // Hover over "Message" in the return-type position.
        let position = Position::new(2, 38);

        let hover = get_hover_info_with_project(
            chat_source,
            &chat_uri,
            position,
            &[(types_uri, types_source.to_string())],
        );

        let hover = hover.expect("cross-file struct hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("struct Message"), "got: {text}");
        assert!(text.contains("text"), "got: {text}");
        assert!(text.contains("seq"), "got: {text}");
        // `chat.ids` has no `use` at all - this is the flat-scan fallback,
        // not the `use`-scoped tier, so the note should be there.
        assert!(
            text.contains("no `use` here brings it into scope"),
            "got: {text}"
        );
        // `text: string` is unbounded, so the whole struct's size is too —
        // the direct regression guard for the `other_docs`-retention
        // restructuring that threads a cross-file SizeLookup through.
        assert!(text.contains("variable"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_cross_file_cycle_does_not_hang() {
        // `A` (the active file) refers to `B`, declared only in a second
        // file, which refers back to `A` — nothing else in the codebase
        // guards this shape (comline-core's own cycle check is scoped to
        // one file's units). This must resolve to *something* sane, not
        // hang or panic.
        let a_source = "struct A {\n    b: B\n}\n";
        let b_source = "struct B {\n    a: A\n}\n";

        let a_uri = Url::parse("file:///a.ids").unwrap();
        let b_uri = Url::parse("file:///b.ids").unwrap();

        // Hover over "A" in its own declaration.
        let position = Position::new(0, 8);

        let hover = get_hover_info_with_project(
            a_source,
            &a_uri,
            position,
            &[(b_uri, b_source.to_string())],
        );

        let hover = hover.expect("hover should resolve even through a cross-file cycle");
        let text = hover_text(hover);
        assert!(text.contains("unknown"), "got: {text}");
    }

    #[test]
    fn test_hover_shows_docstring() {
        let source = "/// Hello\nstruct Message {\n    text: string\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "Message" in the declaration itself.
        let position = Position::new(1, 8);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("Hello"), "got: {text}");
    }

    #[test]
    fn test_hover_on_signed_int_keyword() {
        let source = "struct Sample {\n    value: s16\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "s16" on line 1.
        let position = Position::new(1, 12);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        // Now the specific vocabulary description, not a generic category
        // — "16-bit signed integer" rather than "integer type".
        assert!(text.contains("16-bit signed integer"), "got: {text}");
    }

    #[test]
    fn test_hover_on_type_alias_shows_signature() {
        let source = "type UserId = u64\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "UserId" in the declaration itself.
        let position = Position::new(0, 7);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("type UserId") && text.contains("u64"), "got: {text}");
    }

    #[test]
    fn test_hover_on_type_alias_chain_shows_fully_resolved_type() {
        let source = "type A = B\ntype B = u32\nstruct X {\n    id: A\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        // Hover over "A" in its own declaration ("type A = B", "A" at
        // column 5).
        let position = Position::new(0, 5);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        // The one-level signature names "B"; the fully-resolved chain
        // should also surface "u32" since it differs from the written
        // target.
        assert!(text.contains("type A = B"), "got: {text}");
        assert!(text.contains("u32"), "got: {text}");
    }

    #[test]
    fn use_types_message_resolves_silently_no_note() {
        let chat_source = "use types::Message\n\nprotocol Chat {\n    function send() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let chat_uri = Url::parse("file:///chat.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // Hover over "Message" in the return-type position.
        let position = Position::new(3, 24);

        let hover = get_hover_info_with_project(
            chat_source,
            &chat_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct Message"), "got: {text}");
        assert!(
            !text.contains("no `use` here brings it into scope"),
            "a real `use types::Message` should resolve silently, got: {text}"
        );
    }

    #[test]
    fn two_siblings_declaring_the_same_name_use_picks_the_imported_one() {
        // The actual bug item 4 of this whole plan exists to fix: a flat,
        // first-match scan can't tell `a.ids`'s `Message` from `b.ids`'s -
        // it just returns whichever file happened to come first in
        // `other_files`. `use b::Message` must resolve to `b`'s.
        let active_source = "use b::Message\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let a_source = "struct Message {\n    from_a: bool\n}\n";
        let b_source = "struct Message {\n    from_b: bool\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let a_uri = Url::parse("file:///a.ids").unwrap();
        let b_uri = Url::parse("file:///b.ids").unwrap();

        // Hover over "Message" in the return-type position. `a.ids` comes
        // *before* `b.ids` in `other_files` - a flat scan would find it
        // first and get this wrong.
        let position = Position::new(3, 20);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(a_uri, a_source.to_string()), (b_uri, b_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("from_b"), "got: {text}");
        assert!(!text.contains("from_a"), "got: {text}");
        assert!(!text.contains("no `use` here brings it into scope"), "got: {text}");
    }

    #[test]
    fn use_multi_resolves_a_listed_item_silently() {
        let active_source =
            "use types::{Message, Other}\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\nstruct Other {\n    n: u8\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();
        let position = Position::new(3, 24);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct Message"), "got: {text}");
        assert!(!text.contains("no `use` here brings it into scope"), "got: {text}");
    }

    #[test]
    fn use_glob_resolves_anything_in_the_namespace_silently() {
        let active_source =
            "use types::*\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();
        let position = Position::new(3, 24);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct Message"), "got: {text}");
        assert!(!text.contains("no `use` here brings it into scope"), "got: {text}");
    }

    #[test]
    fn use_as_alias_resolves_the_aliased_name_to_the_real_declaration() {
        let active_source =
            "use types::Message as Msg\n\nprotocol P {\n    function f() -> Msg;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();
        // Hover over "Msg" (the alias) in the return-type position.
        let position = Position::new(3, 20);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("alias-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        // The real declaration is `struct Message`, not `struct Msg` -
        // `types.ids`'s own symbol table never heard of "Msg".
        assert!(text.contains("struct Message"), "got: {text}");
        assert!(!text.contains("no `use` here brings it into scope"), "got: {text}");
    }

    #[test]
    fn use_std_import_falls_back_silently_with_no_note() {
        // `std::` never matches a local sibling (there's no "std" file in
        // this project), and a coincidental same-named local struct isn't
        // the author forgetting a `use` - it's a different symbol
        // entirely, so the fallback must stay quiet here, not suggest a
        // fix that wouldn't apply.
        let active_source =
            "use std::collections::HashMap\n\nstruct S {\n    m: HashMap\n}\n";
        let unrelated_source = "struct HashMap {\n    unrelated: bool\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let unrelated_uri = Url::parse("file:///unrelated.ids").unwrap();
        // Hover over "HashMap" in the field-type position.
        let position = Position::new(3, 9);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(unrelated_uri, unrelated_source.to_string())],
        )
        .expect("flat-scan fallback should still resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct HashMap"), "got: {text}");
        assert!(
            !text.contains("no `use` here brings it into scope"),
            "a std:: import coincidentally sharing a name shouldn't get the not-use-scoped note, got: {text}"
        );
    }
}
