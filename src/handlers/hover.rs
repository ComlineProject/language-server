// Hover handler - provides type information on hover

use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use comline_core::schema::idl::grammar::{Declaration, Document, Expression, Field, Type};
use comline_core::schema::idl::size::{self, SizeLookup, SizeTarget, WireSize};
use lsp_types::{Hover, HoverContents, MarkedString, Position, Url};

/// Get hover information at a position, considering only this file.
pub fn get_hover_info(source: &str, uri: &Url, position: Position) -> Option<Hover> {
    get_hover_info_with_project(source, uri, position, &[])
}

/// Get hover information at a position, also searching `other_files` (every
/// other file in the project, as `(uri, source)` pairs) for a struct/enum/
/// protocol/const the hovered word might name if it isn't declared in this
/// file. This is a flat, project-wide name lookup — first match wins — not a
/// `use`-scoped resolution (mirrors the same simplification the playground's
/// `describe_project` already uses for cross-file type references). Good
/// enough to answer "what is this token", not a claim that it's actually
/// imported here.
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

    // Parse every sibling file once, up front, and keep them all alive for
    // the rest of this call — not just whichever one happens to match the
    // hovered word. A struct's fields can reference types declared in *any*
    // other file, so size computation needs the whole set, not just the
    // document the matched symbol itself lives in.
    let other_docs: Vec<(&Url, &String, Document)> = other_files
        .iter()
        .filter_map(|(u, s)| parser::parse(s).ok()?.document.map(|d| (u, s, d)))
        .collect();

    let lookup = HoverSizeLookup { local: &document, others: &other_docs };

    // Check if it's a symbol in this file
    if let Some(symbol) = symbol_table.get(&word) {
        return Some(create_symbol_hover(symbol, &document, &lookup));
    }

    // Check if it's a symbol declared in another project file
    for (other_uri, other_source, other_document) in &other_docs {
        let other_table = symbols::build_symbol_table(other_document, other_uri, other_source);
        if let Some(symbol) = other_table.get(&word) {
            return Some(create_symbol_hover(symbol, other_document, &lookup));
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

/// Resolves a bare type name to its declaration across the active document
/// plus every other project file — a flat, first-match scan (local document
/// first, then `others` in order), the same simplification already used for
/// cross-file *symbol* hover above. Backs [`size::size_of_type`] /
/// [`size::size_of_struct`] for the size block in [`create_symbol_hover`].
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

/// Format a type for display
fn format_type(ty: &Type) -> String {
    match ty {
        Type::S8(_) => "s8".to_string(),
        Type::S16(_) => "s16".to_string(),
        Type::S32(_) => "s32".to_string(),
        Type::S64(_) => "s64".to_string(),
        Type::U8(_) => "u8".to_string(),
        Type::U16(_) => "u16".to_string(),
        Type::U32(_) => "u32".to_string(),
        Type::U64(_) => "u64".to_string(),
        Type::F32(_) => "f32".to_string(),
        Type::F64(_) => "f64".to_string(),
        Type::Bool(_) => "bool".to_string(),
        Type::Str(_) => "str".to_string(),
        Type::String(_) => "string".to_string(),
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
    // Check if it's a primitive type
    match word {
        "s8" | "s16" | "s32" | "s64" => Some("integer type"),
        "u8" | "u16" | "u32" | "u64" => Some("unsigned integer type"),
        "f32" | "f64" => Some("floating point type"),
        "bool" => Some("boolean type"),
        "str" | "string" => Some("string type"),
        _ => {
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
                .map(|a| format!("@{}={}", a.key(), a.value()))
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
        assert!(text.contains("integer type"), "got: {text}");
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
}
