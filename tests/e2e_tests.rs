// End-to-end test demonstrating the parse → diagnostics flow

use comline_language_server::analysis::diagnostics;
use comline_language_server::parser;
use lsp_types::DiagnosticSeverity;

#[test]
fn test_e2e_document_with_errors() {
    let source = r#"
struct User {
    name string  // Missing colon - syntax error!
    age: i32
}
"#;

    let result = parser::parse(source).unwrap();
    assert!(result.has_errors(), "Should detect syntax error");

    let diagnostics = diagnostics::generate_diagnostics(source, &result.errors);
    assert!(!diagnostics.is_empty(), "Should generate diagnostics");
    assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
}

#[test]
fn test_e2e_valid_document() {
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

    let result = parser::parse(source).unwrap();
    assert!(result.is_ok(), "Should parse successfully");
    assert!(!result.has_errors(), "Should have no errors");

    let doc = result.document.unwrap();
    assert_eq!(parser::get_declaration_count(&doc), 3);

    let diagnostics = diagnostics::generate_diagnostics(source, &result.errors);
    assert!(diagnostics.is_empty(), "Should have no diagnostics for valid code");
}
