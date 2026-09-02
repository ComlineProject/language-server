// Formatting handler - formats Comline code

use lsp_types::{Position, Range, TextEdit};

/// Format an entire document
pub fn format_document(source: &str) -> Vec<TextEdit> {
    // Basic formatting: normalize whitespace and indentation
    let formatted = basic_format(source);
    
    // Return a single edit that replaces entire document
    if formatted == source {
        vec![] // No changes needed
    } else {
        vec![TextEdit {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(u32::MAX, u32::MAX), // End of document
            },
            new_text: formatted,
        }]
    }
}

/// Basic formatting logic
fn basic_format(source: &str) -> String {
    let mut result = String::new();
    let mut indent_level: u32 = 0;
    const INDENT: &str = "    "; // 4 spaces
    
    for line in source.lines() {
        let trimmed = line.trim();
        
        // Skip empty lines
        if trimmed.is_empty() {
            result.push('\n');
            continue;
        }
        
        // Decrease indent before closing brace
        if trimmed.starts_with('}') {
            indent_level = indent_level.saturating_sub(1);
        }
        
        // Add indentation
        for _ in 0..indent_level {
            result.push_str(INDENT);
        }
        
        // Add the line
        result.push_str(trimmed);
        result.push('\n');
        
        // Increase indent after opening brace
        if trimmed.ends_with('{') {
            indent_level += 1;
        }
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_format_struct() {
        let source = "struct User {\nname: string\nage: i32\n}";
        let formatted = basic_format(source);
        
        assert!(formatted.contains("    name: string"));
        assert!(formatted.contains("    age: i32"));
    }
    
    #[test]
    fn test_format_nested() {
        let source = "protocol API {\nfunction test() -> bool;\n}";
        let formatted = basic_format(source);
        
        assert!(formatted.contains("    function test()"));
    }
    
    #[test]
    fn test_format_no_changes() {
        let source = "struct User {\n    name: string\n}\n";
        let edits = format_document(source);
        
        // Should return no edits if already formatted
        assert!(edits.is_empty() || edits[0].new_text == source);
    }
}
