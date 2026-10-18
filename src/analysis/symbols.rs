// Symbol table building from AST

use comline_core::schema::idl::grammar::{Declaration, Document};
use std::collections::HashMap;
use crate::util::{byte_range_to_lsp_range, word_occurrences};
use lsp_types::{Location, SymbolKind, Url};

/// Which namespace a declared name lives in.
///
/// Core keeps one namespace for everything that generates a type or is
/// referenced by name - struct, enum, protocol, const, validator, type
/// alias and error - so a collision among those is a build error. A
/// `settings` block generates nothing and is named only by
/// `@settings = Name`, so it gets its own: `settings Test` beside a
/// `struct Test` is legal, and the editor has to tell the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Declaration,
    SettingsPreset,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Which namespace this name lives in. Two symbols may share a name
    /// across namespaces, so a by-name lookup needs this to pick one.
    pub namespace: Namespace,
    pub location: Location,
    /// Byte range of the declaration's own name - the same span
    /// `location.range` describes, kept so a cursor offset can be tested
    /// directly instead of converting back from line/character.
    pub name_span: (usize, usize),
    /// Byte range of the whole declaration - docstring, annotations, body
    /// and all. What core's own diagnostics span, and what a "delete this
    /// one" quick fix has to remove.
    pub decl_span: (usize, usize),
    /// For structs/protocols: list of field/function names
    pub children: Vec<String>,
}

/// Every declared name in one document, in declaration order.
///
/// Stored as a list and indexed by name, rather than keyed by name. It was
/// a `HashMap<String, Symbol>` whose `insert` overwrote, so a file
/// declaring `struct Test`, `error Test` and `settings Test` kept only the
/// last: hover on any of the three showed the settings block, and the
/// outline listed a single entry sitting at the first declaration's
/// position while carrying the last one's kind and location.
pub struct SymbolTable {
    symbols: Vec<Symbol>,
    /// Name -> indices into `symbols`, in declaration order.
    by_name: HashMap<String, Vec<usize>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self { symbols: Vec::new(), by_name: HashMap::new() }
    }

    pub fn insert(&mut self, symbol: Symbol) {
        self.by_name.entry(symbol.name.clone()).or_default().push(self.symbols.len());
        self.symbols.push(symbol);
    }

    /// The declaration whose own name sits under `offset`.
    ///
    /// How a cursor on a declaration resolves: by position, with no name
    /// lookup to be ambiguous about. Correct even for a collision core has
    /// not rejected, and for one it has - the editor still has to describe
    /// a file it is mid-edit on.
    pub fn at_offset(&self, offset: usize) -> Option<&Symbol> {
        self.index_at_offset(offset).map(|i| &self.symbols[i])
    }

    /// As [`SymbolTable::at_offset`], as an index.
    pub fn index_at_offset(&self, offset: usize) -> Option<usize> {
        self.symbols
            .iter()
            .position(|s| offset >= s.name_span.0 && offset <= s.name_span.1)
    }

    /// What `name` refers to in `namespace` - how a *reference* resolves,
    /// the namespace coming from what the reference's position allows.
    pub fn get_in(&self, name: &str, namespace: Namespace) -> Option<&Symbol> {
        self.index_in(name, namespace).map(|i| &self.symbols[i])
    }

    /// As [`SymbolTable::get_in`], as an index - what a cross-document
    /// reference stores, so it names one symbol and not just a string.
    pub fn index_in(&self, name: &str, namespace: Namespace) -> Option<usize> {
        self.by_name
            .get(name)?
            .iter()
            .copied()
            .find(|&i| self.symbols[i].namespace == namespace)
    }

    pub fn by_index(&self, index: usize) -> Option<&Symbol> {
        self.symbols.get(index)
    }

    /// The name range of the declaration core spanned as `decl_span`.
    ///
    /// Core spans a declaration whole - it has no choice, since
    /// `grammar::Identifier` carries no span of its own - which makes a
    /// duplicate-declaration error underline two entire blocks. The editor
    /// does have the name ranges, so it narrows core's span back down to
    /// the thing that is actually wrong.
    pub fn name_span_of_decl(&self, decl_span: (usize, usize)) -> Option<(usize, usize)> {
        self.symbols.iter().find(|s| s.decl_span == decl_span).map(|s| s.name_span)
    }

    /// Every group of two or more symbols sharing a name *and* a namespace -
    /// a collision core rejects (or, for two settings presets, one it
    /// reports separately). Declaration order is preserved within a group,
    /// so the first entry is the one that holds the name.
    pub fn duplicate_groups(&self) -> Vec<Vec<usize>> {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for indices in self.by_name.values() {
            for namespace in [Namespace::Declaration, Namespace::SettingsPreset] {
                let group: Vec<usize> = indices
                    .iter()
                    .copied()
                    .filter(|&i| self.symbols[i].namespace == namespace)
                    .collect();
                if group.len() > 1 {
                    groups.push(group);
                }
            }
        }
        groups.sort();
        groups
    }

    /// Every symbol, declaration order preserved - including same-named
    /// ones, which the outline previously collapsed into one.
    pub fn all_symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Which namespace a name written at `offset` is a reference in.
///
/// Exactly one form names a settings preset - the value of an `@settings`
/// annotation (`@settings = Strict`). Every other position where a name
/// appears is the declaration namespace: a field's type, a function's
/// argument or return, a `! Name` throws, a `use` line, a validator call.
///
/// Scanned off the source rather than the AST because the grammar's
/// `Identifier` carries only its text, with no span to match an offset
/// against - the same reason `declaration_name_span` locates a name within
/// its declaration's span.
pub fn namespace_at(source: &str, offset: usize) -> Namespace {
    let Some((start, _)) = crate::util::word_range_at(source, offset) else {
        return Namespace::Declaration;
    };

    let before = source[..start].trim_end();
    let Some(before) = before.strip_suffix('=') else {
        return Namespace::Declaration;
    };

    let before = before.trim_end();
    if before.ends_with("@settings") {
        Namespace::SettingsPreset
    } else {
        Namespace::Declaration
    }
}

/// Build symbol table from parsed AST
pub fn build_symbol_table(document: &Document, uri: &Url, source: &str) -> SymbolTable {
    let mut table = SymbolTable::new();

    // Walk through all declarations (each is `Spanned<Declaration>` — deref to match)
    for declaration in &document.0 {
        // Per declaration: its keyword (to find its own name in the
        // source), the LSP kind to report, which namespace the name lives
        // in, and its member names. Several of the kind choices are
        // stand-ins, noted where they are made.
        let (keyword, name, kind, namespace, children): (_, _, _, _, Vec<String>) =
            match &**declaration {
                Declaration::Struct(s) => (
                    "struct",
                    s.name(),
                    SymbolKind::STRUCT,
                    Namespace::Declaration,
                    s.fields().iter().map(|f| f.name()).collect(),
                ),
                Declaration::Enum(e) => (
                    "enum",
                    e.name(),
                    SymbolKind::ENUM,
                    Namespace::Declaration,
                    e.variants().iter().map(|v| v.identifier().text.clone()).collect(),
                ),
                Declaration::Protocol(p) => (
                    "protocol",
                    p.name(),
                    SymbolKind::INTERFACE,
                    Namespace::Declaration,
                    p.functions().iter().map(|f| f.name()).collect(),
                ),
                Declaration::Const(c) => {
                    ("const", c.name(), SymbolKind::CONSTANT, Namespace::Declaration, vec![])
                }
                // `TYPE_PARAMETER` is the closest LSP-standard fit for a
                // transparent type alias - LSP 3.17's `SymbolKind` has no
                // dedicated "type alias" entry, and other LSPs (e.g.
                // rust-analyzer, for Rust's own `type`) use this same kind.
                Declaration::TypeAlias(t) => {
                    ("type", t.name(), SymbolKind::TYPE_PARAMETER, Namespace::Declaration, vec![])
                }
                // `EVENT` is the closest LSP-standard fit: a named, raised/
                // caught thing, same spirit as `TYPE_PARAMETER` standing in
                // for a type alias above - LSP 3.17 has no dedicated
                // "error"/"exception" kind.
                Declaration::Error(e) => (
                    "error",
                    e.name(),
                    SymbolKind::EVENT,
                    Namespace::Declaration,
                    e.fields().iter().map(|f| f.name()).collect(),
                ),
                // `FUNCTION`: a validator is referenced like one is called
                // (`StringBounds(min=3, max=10)`), parameters and all.
                Declaration::Validator(v) => (
                    "validator",
                    v.name(),
                    SymbolKind::FUNCTION,
                    Namespace::Declaration,
                    v.properties().iter().map(|p| p.name()).collect(),
                ),
                Declaration::Settings(s) => {
                    // An unnamed settings block applies implicitly to its
                    // own file - not a named symbol for the outline/
                    // go-to-symbol.
                    let Some(name) = s.name() else { continue };
                    // `OBJECT`: a named object of key/value entries. Its own
                    // namespace - a settings preset generates no type, so it
                    // may share a name with any declaration.
                    (
                        "settings",
                        name,
                        SymbolKind::OBJECT,
                        Namespace::SettingsPreset,
                        s.entries().iter().map(|e| e.key()).collect(),
                    )
                }
                Declaration::Import(_) | Declaration::Use(_) => {
                    // Not a declaration that can be referred to by name.
                    continue;
                }
            };

        let name_span = declaration_name_span(source, declaration.span, keyword, &name);
        table.insert(Symbol {
            name,
            kind,
            namespace,
            location: Location {
                uri: uri.clone(),
                range: byte_range_to_lsp_range(source, name_span.0, name_span.1),
            },
            name_span,
            decl_span: declaration.span,
            children,
        });
    }

    table
}

/// The byte range of a declaration's own name: the first whole-word `name`
/// inside the declaration's span that directly follows its `keyword`
/// (`struct User`) - not just the first `User` anywhere in the file, which
/// could be inside `UserProfile`, a `use` line, or a docstring. Rename
/// edits this range and a cursor is matched against it, so it has to be
/// exact.
fn declaration_name_span(
    source: &str,
    span: (usize, usize),
    keyword: &str,
    name: &str,
) -> (usize, usize) {
    let (start, end) = (span.0.min(source.len()), span.1.min(source.len()));
    let text = &source[start..end];

    let offset = word_occurrences(text, name)
        .into_iter()
        .find(|&i| {
            let before = text[..i].trim_end();
            word_occurrences(before, keyword).last().is_some_and(|&k| k + keyword.len() == before.len())
        })
        .map(|i| start + i)
        .unwrap_or(start);

    (offset, offset + name.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    #[test]
    fn test_build_symbol_table() {
        let source = r#"
struct User {
    name: string
    age: i32
}

enum Role {
    Admin
    User
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();
        assert!(result.is_ok());
        
        let table = build_symbol_table(&result.document.unwrap(), &uri, source);
        
        assert_eq!(table.len(), 2);
        assert!(table.get_in("User", Namespace::Declaration).is_some());
        assert!(table.get_in("Role", Namespace::Declaration).is_some());
        
        let user_symbol = table.get_in("User", Namespace::Declaration).unwrap();
        assert_eq!(user_symbol.kind, SymbolKind::STRUCT);
        assert_eq!(user_symbol.children.len(), 2);
        assert!(user_symbol.children.contains(&"name".to_string()));
        assert!(user_symbol.children.contains(&"age".to_string()));
        
        let role_symbol = table.get_in("Role", Namespace::Declaration).unwrap();
        assert_eq!(role_symbol.kind, SymbolKind::ENUM);
        assert_eq!(role_symbol.children.len(), 2);
    }
    
    #[test]
    fn test_symbol_table_with_type_alias() {
        let source = "type UserId = u64\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();

        let table = build_symbol_table(&result.document.unwrap(), &uri, source);

        assert_eq!(table.len(), 1);
        let alias = table.get_in("UserId", Namespace::Declaration).unwrap();
        assert_eq!(alias.kind, SymbolKind::TYPE_PARAMETER);
        assert!(alias.children.is_empty());
    }

    #[test]
    fn test_symbol_table_with_protocol() {
        let source = r#"
protocol UserService {
    function getUser(i64) -> string;
    function createUser(string) -> i64;
}
"#;
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();
        
        let table = build_symbol_table(&result.document.unwrap(), &uri, source);
        
        assert_eq!(table.len(), 1);
        let protocol = table.get_in("UserService", Namespace::Declaration).unwrap();
        assert_eq!(protocol.kind, SymbolKind::INTERFACE);
        assert_eq!(protocol.children.len(), 2);
    }

    #[test]
    fn same_named_units_in_different_namespaces_are_all_kept() {
        // The bug this table was rewritten for: keyed by name, these three
        // collapsed to whichever came last, so hover on the struct showed
        // the settings block and the outline listed one entry.
        let source = "struct Test {\n    a: string\n}\n\n\
                      settings Test {\n    validators.allowed = true\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();

        let table = build_symbol_table(&result.document.unwrap(), &uri, source);

        assert_eq!(table.len(), 2, "both declarations are kept");
        assert_eq!(
            table.get_in("Test", Namespace::Declaration).unwrap().kind,
            SymbolKind::STRUCT
        );
        assert_eq!(
            table.get_in("Test", Namespace::SettingsPreset).unwrap().kind,
            SymbolKind::OBJECT
        );
    }

    #[test]
    fn a_cursor_on_a_declarations_name_resolves_to_that_declaration() {
        let source = "struct Test {\n    a: string\n}\n\n\
                      settings Test {\n    validators.allowed = true\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();
        let table = build_symbol_table(&result.document.unwrap(), &uri, source);

        // Each `Test` resolves to the declaration it is part of, by
        // position - no name lookup to be ambiguous about.
        let struct_name = source.find("struct Test").unwrap() + "struct ".len();
        assert_eq!(table.at_offset(struct_name).unwrap().kind, SymbolKind::STRUCT);

        let settings_name = source.find("settings Test").unwrap() + "settings ".len();
        assert_eq!(table.at_offset(settings_name).unwrap().kind, SymbolKind::OBJECT);

        // Inside the struct body is not any declaration's own name.
        let body = source.find("a: string").unwrap();
        assert!(table.at_offset(body).is_none());
    }

    #[test]
    fn only_a_settings_annotation_value_is_a_settings_preset_reference() {
        let cases = [
            ("@settings = Test", Namespace::SettingsPreset),
            // Every other position a name appears in.
            ("f: Test", Namespace::Declaration),
            ("function f() ! Test;", Namespace::Declaration),
            ("use types::Test", Namespace::Declaration),
            ("@deprecated = Test", Namespace::Declaration),
        ];
        for (source, expected) in cases {
            let offset = source.rfind("Test").unwrap();
            assert_eq!(namespace_at(source, offset), expected, "{source}");
        }
    }

    #[test]
    fn declaration_range_is_the_declared_name_not_an_earlier_substring() {
        // `User` appears first inside `UserProfile`, then in the `use`
        // line, then in a docstring - none of them are the declaration.
        let source = "use types::User\n\nstruct UserProfile {\n    id: u64\n}\n\n/// The User record\nstruct User {\n    name: string\n}\n";
        let uri = Url::parse("file:///test.ids").unwrap();
        let result = parser::parse(source).unwrap();

        let table = build_symbol_table(&result.document.unwrap(), &uri, source);
        let range = table.get_in("User", Namespace::Declaration).unwrap().location.range;

        assert_eq!(range.start, lsp_types::Position::new(7, 7));
        assert_eq!(range.end, lsp_types::Position::new(7, 11));
    }
}
