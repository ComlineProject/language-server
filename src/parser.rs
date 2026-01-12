use anyhow::Result;
use comline_core::schema::idl::grammar::{self, Document};

/// Parse result containing AST or errors
pub struct ParseResult {
    /// Parsed AST document (if successful)
    pub document: Option<Document>,
    /// Parse errors (if any)
    pub errors: Vec<rust_sitter::errors::ParseError>,
}

impl ParseResult {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
    
    pub fn is_ok(&self) -> bool {
        self.document.is_some() && self.errors.is_empty()
    }
}

/// Parse Comline schema source code
pub fn parse(source: &str) -> Result<ParseResult> {
    tracing::debug!("Parsing {} bytes of source", source.len());
    
    match grammar::parse(source) {
        Ok(document) => {
            tracing::debug!("Parse successful, {} declarations", document.0.len());
            Ok(ParseResult {
                document: Some(document),
                errors: vec![],
            })
        }
        Err(errors) => {
            tracing::debug!("Parse errors: {} error(s)", errors.len());
            Ok(ParseResult {
                document: None,
                errors,
            })
        }
    }
}

/// Get declaration count from a document
pub fn get_declaration_count(doc: &Document) -> usize {
    doc.0.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_struct() {
        let source = r#"
struct User {
    name: string
    age: i32
}
"#;
        let result = parse(source).unwrap();
        assert!(result.is_ok());
        assert_eq!(result.errors.len(), 0);
        let doc = result.document.unwrap();
        assert_eq!(get_declaration_count(&doc), 1);
    }
    
    #[test]
    fn test_parse_enum() {
        let source = r#"
enum Status {
    Active
    Inactive
}
"#;
        let result = parse(source).unwrap();
        assert!(result.is_ok());
        let doc = result.document.unwrap();
        assert_eq!(get_declaration_count(&doc), 1);
    }
    
    #[test]
    fn test_parse_protocol() {
        let source = r#"
protocol UserService {
    function getUser(i64) -> string;
}
"#;
        let result = parse(source).unwrap();
        assert!(result.is_ok());
        let doc = result.document.unwrap();
        assert_eq!(get_declaration_count(&doc), 1);
    }
    
    #[test]
    fn test_parse_error() {
        let source = r#"
struct User {
    name string  // Missing colon
}
"#;
        let result = parse(source).unwrap();
        assert!(result.has_errors());
        assert!(result.document.is_none());
    }
    
    #[test]
    fn test_parse_multiple_declarations() {
        let source = r#"
struct User {
    name: string
}

enum Role {
    Admin
    User
}

protocol Service {
    function test() -> bool;
}
"#;
        let result = parse(source).unwrap();
        assert!(result.is_ok());
        let doc = result.document.unwrap();
        assert_eq!(get_declaration_count(&doc), 3);
    }
}
