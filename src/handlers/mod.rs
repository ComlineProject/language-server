pub mod code_actions;
pub mod completion;
pub mod definition;
pub mod formatting;
pub mod references;
pub mod rename;
pub mod semantic_tokens;
pub mod signature_help;
pub mod symbols;
pub mod annotation_hover;
pub mod comline_toml;
pub mod hovers;
pub mod protocol_summary;
pub mod unit_hover;

use comline_core::settings::catalog;
use lsp_types::{Documentation, Hover, HoverContents, MarkedString, MarkupContent, MarkupKind};

/// A hover of these blocks, over no particular range.
pub(crate) fn hover(contents: Vec<MarkedString>) -> Hover {
    Hover {
        contents: HoverContents::Array(contents),
        range: None,
    }
}

/// A block of Comline source.
pub(crate) fn code(text: String) -> MarkedString {
    MarkedString::from_language_code("comline".to_string(), text)
}

/// The settings shapes enforcement recognizes, one bullet each - what a
/// hover on a settings block (`.ids`) or group (`.idp`) lists.
pub(crate) fn recognized_keys() -> MarkedString {
    let lines: Vec<String> = catalog::block_summary()
        .iter()
        .map(|f| format!("- `{}` - {}", f.key_pattern, f.summary))
        .collect();
    MarkedString::from_markdown(format!("**recognized keys**\n{}", lines.join("\n")))
}

/// Markdown documentation for a completion item.
pub(crate) fn markdown(text: impl Into<String>) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: text.into(),
    })
}
