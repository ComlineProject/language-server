// Basic test fixtures for the LSP server

use tower_lsp::lsp_types::*;

/// Create a simple test document
pub fn create_test_document() -> (Url, String) {
    let url = Url::parse("file:///test.ids").unwrap();
    let content = r#"struct User {
    name: string
    age: i32
}

enum UserRole {
    Admin
    User
}
"#;
    (url, content.to_string())
}

/// Create a document with syntax errors
pub fn create_error_document() -> (Url, String) {
    let url = Url::parse("file:///error.ids").unwrap();
    let content = r#"struct User {
    name string  // Missing colon
    age: i32
}
"#;
    (url, content.to_string())
}

/// Create a protocol document
pub fn create_protocol_document() -> (Url, String) {
    let url = Url::parse("file:///service.ids").unwrap();
    let content = r#"use std::validators::*

struct Request {
    id: i64
    data: string
}

struct Response {
    success: bool
    message: string
}

protocol MyService {
    function process(Request) -> Response;
    function ping() -> bool;
}
"#;
    (url, content.to_string())
}
