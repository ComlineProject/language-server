//! Hover on a protocol's name, or on its `@framing` or `@handshake`: what it
//! resolves to on the wire, where each choice came from, and which
//! guarantees it keeps. The same summary as `comline inspect`, from the same
//! model (`comline_core::inspect`).
//!
//! It needs the package compiled, as `comline inspect` does: the package's
//! `wire` and `handshake` defaults and the encodings it can see all count.
//! The package is compiled from disk, with nothing fetched, so it shows the
//! last saved state; a package that doesn't compile that way shows nothing.

use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use comline_core::inspect::{self, Acceptance, ProtocolSummary, Source};
use comline_core::schema::idl::grammar::{Declaration, Document};
use lsp_types::{Hover, HoverContents, MarkedString, Position, Url};

use crate::util::position_to_offset;

/// The protocol a hover at `offset` is about: its name, or an `@framing` /
/// `@handshake` written on it. `word` is the word under the cursor and
/// `annotation_key` whether an `@` precedes it.
pub fn target(document: &Document, source: &str, offset: usize, word: &str, annotation_key: bool) -> Option<String> {
    for declaration in &document.0 {
        let Declaration::Protocol(protocol) = &declaration.value else { continue };
        let (start, end) = declaration.span;
        if offset < start || offset > end {
            continue;
        }
        if annotation_key {
            if matches!(word, "framing" | "handshake") && protocol.annotations().iter().any(|a| a.key.as_str() == word) {
                return Some(protocol.name());
            }
            return None;
        }
        // The name itself, not a function or type of the same name inside.
        let before = source[start..offset.min(source.len())]
            .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
            .trim_end();
        if word == protocol.name() && before.ends_with("protocol") {
            return Some(protocol.name());
        }
    }
    None
}

/// The summary of protocol `name` in schema `schema` (`api`), from the
/// package at `package_dir` compiled from disk.
pub fn summary(package_dir: &Path, schema: &str, name: &str) -> Option<ProtocolSummary> {
    let compiled = panic::catch_unwind(AssertUnwindSafe(|| {
        comline_core::package::build::compile_package_without_fetching(package_dir)
    }));
    let Ok(Ok(context)) = compiled else { return None };
    inspect::protocol(&context, schema, name)
}

/// The summary as markdown: the choices, then a table of guarantees.
pub fn markdown(p: &ProtocolSummary) -> String {
    let mut out = String::from("**On the wire**\n\n| | | from |\n|---|---|---|\n");
    for (key, choice) in [("format", &p.format), ("framing", &p.framing), ("handshake", &p.handshake)] {
        let mut value = format!("`{}`", choice.value);
        if !choice.params.is_empty() {
            let params: Vec<String> = choice.params.iter().map(|(k, v)| format!("{k} = {v}")).collect();
            value.push_str(&format!(" ({})", params.join(", ")));
        }
        let from = match choice.source {
            Source::Annotation => format!("`@{key}`"),
            Source::PackageDefault if key == "handshake" => "config.idp `handshake`".to_string(),
            Source::PackageDefault => format!("config.idp `wire.{key}`"),
            Source::CoreDefault => "core's default".to_string(),
            Source::Framing => "JSON-RPC's format".to_string(),
        };
        out.push_str(&format!("| {key} | {value} | {from} |\n"));
    }

    out.push_str("\n**Guarantees**\n\n| guarantee | | why | accepted |\n|---|---|---|---|\n");
    for row in &p.guarantees {
        let state = if row.held { "held" } else { "**given up**" };
        let why = row.causes.join("; ");
        let accepted = match &row.acceptance {
            Acceptance::NotNeeded => String::new(),
            Acceptance::Unknown => "by each consumer".to_string(),
            Acceptance::Accepted(by) => format!("by {by}"),
            Acceptance::NotAccepted => "**not accepted**".to_string(),
        };
        out.push_str(&format!("| {} | {state} | {} | {accepted} |\n", row.guarantee.describe(), cell(&why)));
    }
    if let Some(reason) = p.guarantees.iter().find_map(|g| g.reason.as_deref()) {
        out.push_str(&format!("\n`@unverified`: \"{}\"\n", reason));
    }
    out
}

/// `hover` with the summary added, when the cursor at `position` in
/// `source` (the file at `uri`) is on a protocol's name or on its `@framing`
/// or `@handshake`, and the package compiles. Otherwise `hover` unchanged.
pub fn extend(hover: Option<Hover>, source: &str, uri: &Url, position: Position) -> Option<Hover> {
    let Some(summary) = (|| {
        let offset = position_to_offset(source, position)?;
        if offset >= source.len() {
            return None;
        }
        let start = source[..offset].rfind(|c: char| !c.is_alphanumeric() && c != '_').map(|i| i + 1).unwrap_or(0);
        let end = source[offset..].find(|c: char| !c.is_alphanumeric() && c != '_').map(|i| offset + i).unwrap_or(source.len());
        let word = &source[start..end];
        let annotation_key = start > 0 && source.as_bytes()[start - 1] == b'@';
        let document = crate::parser::parse(source).ok()?.document?;
        let name = target(&document, source, offset, word, annotation_key)?;
        let package_dir = package_dir(uri)?;
        let schema = crate::analysis::imports::namespace_of(uri).join("::");
        summary(&package_dir, &schema, &name)
    })() else {
        return hover;
    };
    let summary = MarkedString::from_markdown(markdown(&summary));
    let mut parts = match hover {
        None => vec![],
        Some(Hover { contents: HoverContents::Array(parts), .. }) => parts,
        Some(Hover { contents: HoverContents::Scalar(part), .. }) => vec![part],
        Some(Hover { contents: HoverContents::Markup(markup), .. }) => vec![MarkedString::from_markdown(markup.value)],
    };
    parts.push(summary);
    Some(Hover { contents: HoverContents::Array(parts), range: None })
}

/// The package directory a schema file at `uri` belongs to: the parent of
/// its `src/`. None in the browser, where nothing is on disk.
#[cfg(not(target_arch = "wasm32"))]
fn package_dir(uri: &Url) -> Option<std::path::PathBuf> {
    let path = uri.to_file_path().ok()?;
    Some(comline_core::package::layout::schemas_root_for(&path)?.parent()?.to_path_buf())
}

#[cfg(target_arch = "wasm32")]
fn package_dir(_uri: &Url) -> Option<std::path::PathBuf> {
    None
}

/// A table cell: no `|` or line break of its own.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dir: &Path, config: &str, schema: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("config.idp"),
            format!("congregation shop\nspecification_version = 1\n{config}"),
        )
        .unwrap();
        std::fs::write(dir.join("src/api.ids"), schema).unwrap();
    }

    const SCHEMA: &str = "protocol Orders {\n    function place(id: u64) -> bool;\n}\n\n\
        @framing = { name = \"jsonrpc\", method_names = \"qualified\" }\n\
        @handshake = \"none\"\n\
        @unverified = \"the 2009 gateway\"\n\
        protocol Legacy {\n    function Legacy();\n}\n";

    fn at(needle: &str, nth: usize) -> usize {
        SCHEMA.match_indices(needle).nth(nth).unwrap().0 + 1
    }

    fn target_at(offset: usize) -> Option<String> {
        let document = crate::parser::parse(SCHEMA).unwrap().document.unwrap();
        let start = SCHEMA[..offset].rfind(|c: char| !c.is_alphanumeric() && c != '_').map(|i| i + 1).unwrap_or(0);
        let end = SCHEMA[offset..].find(|c: char| !c.is_alphanumeric() && c != '_').map(|i| offset + i).unwrap();
        let annotation_key = start > 0 && SCHEMA.as_bytes()[start - 1] == b'@';
        target(&document, SCHEMA, offset, &SCHEMA[start..end], annotation_key)
    }

    #[test]
    fn a_protocols_name_framing_and_handshake_are_targets() {
        assert_eq!(target_at(at("Orders", 0)).as_deref(), Some("Orders"));
        assert_eq!(target_at(at("Legacy", 0)).as_deref(), Some("Legacy"));
        assert_eq!(target_at(at("framing", 0)).as_deref(), Some("Legacy"));
        assert_eq!(target_at(at("handshake", 0)).as_deref(), Some("Legacy"));
        // Not a protocol's name or its wire annotations.
        assert_eq!(target_at(at("unverified", 0)), None);
        assert_eq!(target_at(at("place", 0)), None);
        assert_eq!(target_at(at("Legacy", 1)), None, "a function named like its protocol");
    }

    #[test]
    fn the_summary_is_a_table_of_choices_and_guarantees() {
        let root = std::env::temp_dir().join(format!("comline-lsp-protocol-summary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        package(
            &root,
            "wire = { framing = \"datagram@1\" }\n\
             settings = { annotations = { unverified = { allowed = true } framing = { allowed = true } } }\n",
            SCHEMA,
        );
        let orders = summary(&root, "api", "Orders").unwrap();
        assert!(markdown(&orders).contains("| framing | `datagram` | config.idp `wire.framing` |"), "{}", markdown(&orders));

        let legacy = summary(&root, "api", "Legacy").unwrap();
        assert_eq!(
            markdown(&legacy),
            "**On the wire**\n\n| | | from |\n|---|---|---|\n\
             | format | `json@1` | JSON-RPC's format |\n\
             | framing | `jsonrpc` (method_names = qualified) | `@framing` |\n\
             | handshake | `none` | `@handshake` |\n\
             \n**Guarantees**\n\n| guarantee | | why | accepted |\n|---|---|---|---|\n\
             | schema agreement | **given up** | handshake `none` | by each consumer |\n\
             | encoding agreement | **given up** | handshake `none` | by each consumer |\n\
             | encoding conformance | held |  |  |\n\
             | version honesty | held |  |  |\n\
             \n`@unverified`: \"the 2009 gateway\"\n"
        );
        assert_eq!(summary(&root, "api", "Missing"), None);

        // Through the hover: on `@handshake`, after what the hover had.
        let uri = Url::from_file_path(root.join("src/api.ids")).unwrap();
        let offset = at("handshake", 0);
        let line = SCHEMA[..offset].matches('\n').count() as u32;
        let character = (offset - SCHEMA[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
        let before = Hover { contents: HoverContents::Array(vec![MarkedString::from_markdown("`@handshake`".into())]), range: None };
        let Some(Hover { contents: HoverContents::Array(parts), .. }) =
            extend(Some(before), SCHEMA, &uri, Position { line, character })
        else {
            panic!("a hover")
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], MarkedString::from_markdown(markdown(&legacy)));
        // Elsewhere, the hover is what it was.
        assert_eq!(extend(None, SCHEMA, &uri, Position { line: 1, character: 14 }), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
