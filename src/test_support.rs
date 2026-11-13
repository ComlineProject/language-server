//! What the crate's unit tests share: a file to stand in for the one being
//! edited, the hover and completion at a position in it, a hover's text,
//! and files written to disk.

use std::path::Path;

use lsp_types::{CompletionItem, Hover, HoverContents, MarkedString, Position, TextEdit, Url};

/// The URI of the schema a test edits.
pub fn test_uri() -> Url {
    Url::parse("file:///test.ids").unwrap()
}

/// The hover at `line`:`character` of `source`, a file on its own.
pub fn hover_at(source: &str, line: u32, character: u32) -> Option<Hover> {
    crate::handlers::hovers::ids::get_hover_info(source, &test_uri(), Position::new(line, character))
}

/// The completions at `line`:`character` of `source`, a file on its own.
pub fn completions_at(source: &str, line: u32, character: u32) -> Vec<CompletionItem> {
    crate::handlers::completion::get_completions(source, &test_uri(), Position::new(line, character))
}

/// Each block of a hover's text, in order; it must be an array of them.
pub fn blocks(hover: Hover) -> Vec<String> {
    match hover.contents {
        HoverContents::Array(parts) => parts.into_iter().map(marked_text).collect(),
        _ => panic!("expected an array of blocks"),
    }
}

/// All of a hover's text, its blocks a line apart.
pub fn hover_text(hover: Hover) -> String {
    match hover.contents {
        HoverContents::Array(parts) => parts
            .into_iter()
            .map(marked_text)
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Scalar(marked) => marked_text(marked),
        HoverContents::Markup(m) => m.value,
    }
}

fn marked_text(marked: MarkedString) -> String {
    match marked {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(ls) => ls.value,
    }
}

/// `text` with `edit` applied. A position's character counts bytes.
pub fn apply_edit(text: &str, edit: &TextEdit) -> String {
    let offset = |p: Position| {
        text.split_inclusive('\n')
            .take(p.line as usize)
            .map(str::len)
            .sum::<usize>()
            + p.character as usize
    };
    format!(
        "{}{}{}",
        &text[..offset(edit.range.start)],
        edit.new_text,
        &text[offset(edit.range.end)..]
    )
}

/// Writes each `(path, text)` under `root` (an absolute path stands on its
/// own), creating the directories it needs.
pub fn write_tree<P: AsRef<Path>, T: AsRef<[u8]>>(
    root: &Path,
    files: impl IntoIterator<Item = (P, T)>,
) {
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
