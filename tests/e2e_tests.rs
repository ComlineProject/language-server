// End-to-end test demonstrating the full LSP flow

use tower_lsp::lsp_types::*;

#[tokio::test]
async fn test_e2e_document_with_errors() {
    use comline_language_server::{backend::Backend, document::DocumentStore};
    use tower_lsp::LspService;
    
    // Create test document with a syntax error
    let source = r#"
struct User {
    name string  // Missing colon - syntax error!
    age: i32
}
"#;
    
    let uri = Url::parse("file:///test.ids").unwrap();
    
    // The backend would parse this and should detect the error
    // We'll just verify our parser catches it
    let result = comline_language_server::parser::parse(source).unwrap();
    assert!(result.has_errors(), "Should detect syntax error");
    
    // Generate diagnostics
    let diagnostics = comline_language_server::analysis::diagnostics::generate_diagnostics(source, &result.errors);
    assert!(!diagnostics.is_empty(), "Should generate diagnostics");
    assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
}

#[tokio::test]
async fn test_e2e_valid_document() {
    let source = r#"
struct User {
    name: string
    age: i32
}

enum Role {
    Admin
    User
    Guest
}

protocol UserService {
    function getUser(i64) -> User;
    function listUsers() -> User[];
}
"#;
    
    // Parse should succeed
    let result = comline_language_server::parser::parse(source).unwrap();
    assert!(result.is_ok(), "Should parse successfully");
    assert!(!result.has_errors(), "Should have no errors");
    
    // Should identify 3 declarations
    let doc = result.document.unwrap();
    assert_eq!(comline_language_server::parser::get_declaration_count(&doc), 3);
    
    // Should generate no diagnostics
    let diagnostics = comline_language_server::analysis::diagnostics::generate_diagnostics(source, &result.errors);
    assert!(diagnostics.is_empty(), "Should have no diagnostics for valid code");
}
