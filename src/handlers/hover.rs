// Hover handler - provides type information on hover

use crate::analysis::symbols;
use crate::parser;
use crate::util::position_to_offset;
use comline_core::schema::idl::grammar::{Declaration, Type};
use lsp_types::{Hover, HoverContents, MarkedString, Position, Url};

/// Get hover information at a position
pub fn get_hover_info(source: &str, uri: &Url, position: Position) -> Option<Hover> {
    // Convert position to byte offset
    let offset = position_to_offset(source, position)?;
    
    // Parse the document
    let parse_result = parser::parse(source).ok()?;
    let document = parse_result.document?;
    
    // Build symbol table
    let symbol_table = symbols::build_symbol_table(&document, uri, source);
    
    // Find what's at this position
    let word = get_word_at_offset(source, offset)?;
    
    // Check if it's a symbol
    if let Some(symbol) = symbol_table.get(&word) {
        return Some(create_symbol_hover(symbol, &document));
    }
    
    // Check if it's a type reference
    if let Some(type_info) = find_type_at_position(&document, &word) {
        return Some(create_type_hover(&word, type_info));
    }
    
    // Check if it's a field reference
    if let Some(field_info) = find_field_info(&document, &word, offset, source) {
        return Some(create_field_hover(&field_info));
    }
    
    None
}

/// Create hover for a symbol (struct, enum, protocol, const)
fn create_symbol_hover(symbol: &symbols::Symbol, document: &comline_core::schema::idl::grammar::Document) -> Hover {
    use lsp_types::SymbolKind;
    
    let mut contents = vec![];
    
    // Add symbol signature
    let signature = match symbol.kind {
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
                
                format!("struct {} {{\n{}\n}}", symbol.name, fields.join("\n"))
            } else {
                format!("struct {}", symbol.name)
            }
        }
        SymbolKind::ENUM => {
            if let Some(e) = find_enum_declaration(document, &symbol.name) {
                let variants: Vec<String> = e.variants()
                    .iter()
                    .map(|v| format!("  {}", v.identifier().text))
                    .collect();
                
                format!("enum {} {{\n{}\n}}", symbol.name, variants.join("\n"))
            } else {
                format!("enum {}", symbol.name)
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
                
                format!("protocol {} {{\n{}\n}}", symbol.name, functions.join("\n"))
            } else {
                format!("protocol {}", symbol.name)
            }
        }
        SymbolKind::CONSTANT => {
            if let Some(c) = find_const_declaration(document, &symbol.name) {
                format!("const {}: {}", c.name(), format_type(c.type_def()))
            } else {
                format!("const {}", symbol.name)
            }
        }
        _ => symbol.name.clone()
    };
    
    contents.push(MarkedString::from_language_code("comline".to_string(), signature));
    
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

/// Create hover for a field
fn create_field_hover(info: &str) -> Hover {
    Hover {
        contents: HoverContents::Scalar(MarkedString::from_language_code(
            "comline".to_string(),
            info.to_string(),
        )),
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
        "i8" | "i16" | "i32" | "i64" => Some("integer type"),
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
                    _ => {}
                }
            }
            None
        }
    }
}

/// Find field info at position
fn find_field_info(_document: &comline_core::schema::idl::grammar::Document, _word: &str, _offset: usize, _source: &str) -> Option<String> {
    // TODO: Implement field lookup
    None
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
}
