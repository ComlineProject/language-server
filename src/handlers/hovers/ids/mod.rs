//! Schema hover handler - provides type information on hover

#[cfg(test)]
mod tests;

use crate::analysis::imports::{self, ProjectFile};
use crate::analysis::modules::{self, Module};
use crate::analysis::project::Project;
use crate::analysis::source::{self, ProjectSource};
use crate::analysis::symbols;
use crate::handlers::annotation_hover::AnnotationHover;
use crate::parser;
use crate::util::{byte_range_to_lsp_range, in_comment_or_string, position_to_offset};
use comline_core::schema::idl::annotations;
use comline_core::schema::idl::grammar::{
    Declaration, Document, Expression, Field, Settings, Type,
};
use comline_core::schema::idl::module_docs::summary;
use comline_core::schema::idl::size::{self, SizeLookup, SizeTarget, WireSize, size_of_type};
use comline_core::schema::idl::vocabulary;
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

    // Cursor is literally inside a `settings { ... }` block's own text
    // (named or unnamed) — checked directly by span, not through the
    // symbol table (which only tracks *named* settings blocks, since an
    // unnamed one has no name to be a symbol under — see `symbols.rs`).
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
            let document = parser::parse(file.text()).ok()?.document?;
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
    };

    // A symbol in this file. The cursor sitting on a declaration's own
    // name resolves by position; anything else is a reference, resolved in
    // whatever namespace its position allows.
    //
    // By name alone, a file declaring `struct Test` beside a
    // `settings Test` answered both with whichever the symbol table had
    // kept - so hovering the struct showed the settings block.
    let own = symbol_table
        .at_offset(offset)
        .or_else(|| symbol_table.get_in(&word, symbols::namespace_at(source, offset)));
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
    if let Some(type_info) = find_type_at_position(&document, &word) {
        return Some(create_type_hover(&word, type_info));
    }

    // Check if it's a field reference
    if let Some(field_info) = find_field_info(&document, &word, offset, &lookup) {
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
/// `others`). A name nothing imports stays unresolved, and its size
/// `Unknown`. Backs
/// [`size::size_of_type`] / [`size::size_of_struct`] for the size block in
/// [`create_symbol_hover`].
struct HoverSizeLookup<'a> {
    local: &'a Document,
    others: &'a [(&'a Url, &'a str, Document)],
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
    others: &[(&Url, &str, Document)],
) -> Type {
    let mut current = ty.clone();
    for _ in 0..16 {
        let Type::Named(id) = &current else { break };
        let name = id.to_string();
        let next = find_type_alias_declaration(local, &name)
            .or_else(|| {
                others
                    .iter()
                    .find_map(|(_, _, doc)| find_type_alias_declaration(doc, &name))
            })
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
            format!(
                "wire size (fixed, raw-packed estimate): {} ({} bits)",
                byte_count(bytes),
                bytes * 8
            )
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
        WireSize::Fixed(bytes) => byte_count(bytes),
        WireSize::Variable => "variable".to_string(),
        WireSize::Unknown => "unknown".to_string(),
    }
}

/// "1 byte" / "N bytes" — 0 reads as plural ("0 bytes"), same as English
/// normally treats a zero count.
fn byte_count(bytes: u32) -> String {
    if bytes == 1 {
        "1 byte".to_string()
    } else {
        format!("{bytes} bytes")
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
fn render_field_sizes(
    s: &comline_core::schema::idl::grammar::Struct,
    lookup: &HoverSizeLookup,
) -> String {
    s.fields()
        .iter()
        .enumerate()
        .map(|(index, f)| {
            format!(
                "- #{index} {}: {}",
                f.name(),
                render_size_oneline(field_size(f, lookup))
            )
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

/// Create hover for a symbol (struct, enum, protocol, const)
fn create_symbol_hover(
    symbol: &symbols::Symbol,
    document: &Document,
    lookup: &HoverSizeLookup,
) -> Hover {
    use lsp_types::SymbolKind;

    let mut contents = vec![];

    // Add symbol signature, its doc-comment when present, and (struct/enum
    // only — where "total size" is meaningful) a wire-size estimate block.
    let (signature, doc, size_block) = match symbol.kind {
        SymbolKind::STRUCT => {
            // Find the struct to get its fields
            if let Some(s) = find_struct_declaration(document, &symbol.name) {
                let fields: Vec<String> = s
                    .fields()
                    .iter()
                    .map(|f| {
                        let opt = if f.optional() { "optional " } else { "" };
                        format!("  {}{}: {}", opt, f.name(), format_type(f.field_type()))
                    })
                    .collect();

                let total = size::size_of_struct(s, lookup);
                let size_block = format!(
                    "{}\n{}",
                    format_wire_size(total),
                    render_field_sizes(s, lookup)
                );

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
                let variants: Vec<String> = e
                    .variants()
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
                let functions: Vec<String> = p
                    .functions()
                    .iter()
                    .map(|f| {
                        let args = if let Some(args_list) = f.args() {
                            let mut arg_types = vec![format_type(args_list.first().arg_type())];
                            arg_types.extend(
                                args_list
                                    .rest()
                                    .iter()
                                    .map(|ca| format_type(ca.arg_type().arg_type())),
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

                (
                    format!("protocol {} {{\n{}\n}}", symbol.name, functions.join("\n")),
                    p.docstring(),
                    None,
                )
            } else {
                (format!("protocol {}", symbol.name), None, None)
            }
        }
        SymbolKind::CONSTANT => {
            if let Some(c) = find_const_declaration(document, &symbol.name) {
                let size = size::size_of_type(&c.type_def, lookup);
                let size_display = format_wire_size(size);

                println!("Size of constant: {}", format_wire_size(size));
                
                /*
                let total = size::size_of_struct(s, lookup);
                let size_block = format!(
                    "{}\n{}",
                    format_wire_size(total),
                    render_field_sizes(s, lookup)
                );
                */

                (
                    format!("const {}: {}", c.name(), format_type(c.type_def())),
                    c.docstring(),
                    Some(size_display),
                )
            } else {
                (format!("const {}", symbol.name), None, None)
            }
        }
        SymbolKind::TYPE_PARAMETER => {
            if let Some(t) = find_type_alias_declaration(document, &symbol.name) {
                let target = t.target_type();
                let target_text = format_type(target);
                // Its annotations above it, as written - an alias's
                // `@validators` run wherever it's used, so they belong in
                // what it is.
                let annotations: String = t
                    .annotations()
                    .iter()
                    .map(|a| match a.value() {
                        Some(value) => format!("@{} = {value}\n", a.key()),
                        None => format!("@{}\n", a.key()),
                    })
                    .collect();
                let signature = format!("{annotations}type {} = {}", symbol.name, target_text);
                let resolved_text = format_type(&fully_resolve_alias_chain(
                    target,
                    lookup.local,
                    lookup.others,
                ));

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
        SymbolKind::EVENT => {
            if let Some(e) = find_error_declaration(document, &symbol.name) {
                let fields: Vec<String> = e
                    .fields()
                    .iter()
                    .map(|f| {
                        let opt = if f.optional() { "optional " } else { "" };
                        format!("  {}{}: {}", opt, f.name(), format_type(f.field_type()))
                    })
                    .collect();
                (
                    format!("error {} {{\n{}\n}}", symbol.name, fields.join("\n")),
                    e.docstring(),
                    None,
                )
            } else {
                (format!("error {}", symbol.name), None, None)
            }
        }
        SymbolKind::FUNCTION => {
            if let Some(v) = find_validator_declaration(document, &symbol.name) {
                let params: Vec<String> = v
                    .properties()
                    .iter()
                    .map(|p| format!("{}: {}", p.name(), format_type(p.property_type())))
                    .collect();
                (
                    format!("validator {}({})", symbol.name, params.join(", ")),
                    v.docstring(),
                    None,
                )
            } else {
                (format!("validator {}", symbol.name), None, None)
            }
        }
        SymbolKind::OBJECT => {
            if let Some(s) = find_settings_declaration(document, &symbol.name) {
                let entries: Vec<String> = s
                    .entries()
                    .iter()
                    .map(|e| format!("  {} = {}", e.key(), e.value_string()))
                    .collect();
                (
                    format!("settings {} {{\n{}\n}}", symbol.name, entries.join("\n")),
                    s.docstring(),
                    None,
                )
            } else {
                (format!("settings {}", symbol.name), None, None)
            }
        }
        _ => (symbol.name.clone(), None, None),
    };

    contents.push(MarkedString::from_language_code(
        "comline".to_string(),
        signature,
    ));
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
            SymbolKind::EVENT => format!("{} fields", symbol.children.len()),
            SymbolKind::FUNCTION => format!("{} parameters", symbol.children.len()),
            SymbolKind::OBJECT => format!("{} settings", symbol.children.len()),
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
/// path already built for named blocks (`SymbolKind::OBJECT`,
/// `find_settings_declaration`) — now also reachable for unnamed ones —
/// plus a "what's recognized here" catalog listing.
fn create_settings_block_hover(settings: &Settings) -> Hover {
    let entries: Vec<String> = settings
        .entries()
        .iter()
        .map(|e| format!("  {} = {}", e.key(), e.value_string()))
        .collect();
    let label = settings.name().unwrap_or_default();
    let signature = format!(
        "settings {}{{\n{}\n}}",
        if label.is_empty() {
            String::new()
        } else {
            format!("{label} ")
        },
        entries.join("\n")
    );

    let mut contents = vec![MarkedString::from_language_code(
        "comline".to_string(),
        signature,
    )];
    if let Some(doc) = settings.docstring() {
        contents.push(MarkedString::from_markdown(doc));
    }

    let catalog_lines: Vec<String> = catalog::block_summary()
        .iter()
        .map(|f| format!("- `{}` - {}", f.key_pattern, f.summary))
        .collect();
    contents.push(MarkedString::from_markdown(format!(
        "**recognized keys**\n{}",
        catalog_lines.join("\n")
    )));

    contents.push(MarkedString::from_markdown(format!(
        "*{} settings*",
        settings.entries().len()
    )));

    Hover {
        contents: HoverContents::Array(contents),
        range: None,
    }
}

/// Entry-level: the raw as-written `key = value` (unchanged content), a
/// catalog match for what this key actually controls (if it matches a
/// shape `settings::enforcement` recognizes), and the resolved effective
/// value after package+schema(+preset) layering — not just the raw one.
fn create_settings_entry_hover<S: ProjectSource>(
    settings: &Settings,
    entry: &rust_sitter::Spanned<comline_core::schema::idl::grammar::Setting>,
    document: &Document,
    other_files: &[S],
) -> Hover {
    let key = entry.key();
    let mut contents = vec![MarkedString::from_language_code(
        "comline".to_string(),
        format!("{} = {}", key, entry.value_string()),
    )];

    let key_path = entry.key_path();
    let segments: Vec<&str> = key_path.iter().map(String::as_str).collect();
    if let Some(m) = catalog::match_path(&segments) {
        contents.push(MarkedString::from_markdown(
            catalog::doc_for(&m).description.to_string(),
        ));
    }

    let package_settings = other_files
        .iter()
        .find(|f| f.dependency().is_none() && source::is_manifest(f.uri()))
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
    contents.push(MarkedString::from_language_code(
        "comline".to_string(),
        signature,
    ));

    if let Some(doc) = &info.docstring {
        contents.push(MarkedString::from_markdown(doc.clone()));
    }

    let mut detail = vec![
        format!(
            "field **#{}** of `{}` ({} field{})",
            info.index,
            info.container,
            info.total,
            if info.total == 1 { "" } else { "s" }
        ),
        format!("size: {}", render_size_oneline(info.size)),
    ];
    detail.push(format!(
        "optional: {}",
        if info.optional { "yes" } else { "no" }
    ));
    if !info.annotations.is_empty() {
        detail.push(format!("annotations: {}", info.annotations.join(", ")));
    }
    contents.push(MarkedString::from_markdown(detail.join("\n\n")));

    Hover {
        contents: HoverContents::Array(contents),
        range: None,
    }
}

/// A size as written: `N` (exactly N) or `max N` (at most N, a bound),
/// N being an integer or a constant's name.
fn format_size(size: &comline_core::schema::idl::grammar::ArraySize) -> String {
    use comline_core::schema::idl::grammar::{ArraySize, SizeValue};
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
fn find_type_at_position(
    document: &comline_core::schema::idl::grammar::Document,
    word: &str,
) -> Option<&'static str> {
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
        Expression::Bool(b) => b.as_text().to_string(),
        Expression::Integer(i) => i.value.to_string(),
        Expression::String(s) => format!("\"{}\"", s.value),
        Expression::FString(f) => f.source(),
        Expression::Path(p) => p.text.clone(),
        Expression::Identifier(i) => i.text.clone(),
    }
}

// Helper functions to find declarations
fn find_struct_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Struct> {
    for decl in &document.0 {
        if let Declaration::Struct(s) = &**decl {
            if s.name() == name {
                return Some(s);
            }
        }
    }
    None
}

fn find_enum_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Enum> {
    for decl in &document.0 {
        if let Declaration::Enum(e) = &**decl {
            if e.name() == name {
                return Some(e);
            }
        }
    }
    None
}

fn find_protocol_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Protocol> {
    for decl in &document.0 {
        if let Declaration::Protocol(p) = &**decl {
            if p.name() == name {
                return Some(p);
            }
        }
    }
    None
}

fn find_const_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Const> {
    for decl in &document.0 {
        if let Declaration::Const(c) = &**decl {
            if c.name() == name {
                return Some(c);
            }
        }
    }
    None
}

fn find_type_alias_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::TypeAlias> {
    for decl in &document.0 {
        if let Declaration::TypeAlias(t) = &**decl {
            if t.name() == name {
                return Some(t);
            }
        }
    }
    None
}

fn find_error_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Error> {
    for decl in &document.0 {
        if let Declaration::Error(e) = &**decl {
            if e.name() == name {
                return Some(e);
            }
        }
    }
    None
}

fn find_validator_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Validator> {
    for decl in &document.0 {
        if let Declaration::Validator(v) = &**decl {
            if v.name() == name {
                return Some(v);
            }
        }
    }
    None
}

fn find_settings_declaration<'a>(
    document: &'a comline_core::schema::idl::grammar::Document,
    name: &str,
) -> Option<&'a comline_core::schema::idl::grammar::Settings> {
    for decl in &document.0 {
        if let Declaration::Settings(s) = &**decl {
            if s.name().as_deref() == Some(name) {
                return Some(s);
            }
        }
    }
    None
}
