//! Hover on a protocol's name, or on its `@framing` or `@handshake`: what it
//! resolves to on the wire, where each choice came from, and which
//! guarantees it keeps. The same summary as `comline inspect`, from the same
//! model (`comline_core::inspect`).
//!
//! It needs the package compiled, as `comline inspect` does: the package's
//! `wire` and `handshake` defaults and the encodings it can see all count.
//! The package is compiled from disk, with nothing fetched, so it shows the
//! last saved state; a package that doesn't compile that way shows nothing.
//!
//! The saved state is not the buffer's: an unsaved `@handshake = "none"`, or
//! a removed `@unverified`, changes what is guaranteed and the saved build
//! doesn't know. So when any open file the build reads (a schema of the
//! package, or its `config.idp`) differs from disk, the guarantee rows are
//! left out and a line says why - never a guarantee claimed from a stale
//! view. The rest of the summary stays (see [`markdown_unsaved`]).

use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use comline_core::inspect::{self, Acceptance, ProtocolSummary, Source};
use comline_core::schema::idl::annotations::{self, HandshakeValue};
use comline_core::schema::idl::grammar::{Declaration, Document};
use lsp_types::{Hover, HoverContents, MarkedString, Position, Url};

use crate::util::position_to_offset;

/// The protocol a hover at `offset` is about: its name, or an `@framing` /
/// `@handshake` written on it. `word` is the word under the cursor and
/// `annotation_key` whether an `@` precedes it.
pub fn target(
    document: &Document,
    source: &str,
    offset: usize,
    word: &str,
    annotation_key: bool,
) -> Option<String> {
    for declaration in &document.0 {
        let Declaration::Protocol(protocol) = &declaration.value else {
            continue;
        };
        let (start, end) = declaration.span;
        if offset < start || offset > end {
            continue;
        }
        if annotation_key {
            if matches!(word, "framing" | "handshake")
                && protocol
                    .annotations()
                    .iter()
                    .any(|a| a.key.as_str() == word)
            {
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
    let Ok(Ok(context)) = compiled else {
        return None;
    };
    inspect::protocol(&context, schema, name)
}

/// The summary as markdown: the choices, then a table of guarantees.
pub fn markdown(p: &ProtocolSummary) -> String {
    let mut out = choices(p);
    out.push_str(&guarantees(p));
    out
}

/// The summary for a package whose view differs from disk: the choices, and
/// in place of the guarantees, [`UNSAVED_NOTE`]. The summary is of the saved
/// build, so what it says is not the buffer's to claim held or given up.
pub fn markdown_unsaved(p: &ProtocolSummary) -> String {
    let mut out = choices(p);
    out.push_str(&format!("\n*{UNSAVED_NOTE}*\n"));
    out
}

/// What the hover says in place of the guarantees.
pub const UNSAVED_NOTE: &str = "Guarantees: save to see them. \
    The open file has unsaved changes, and they come from the saved build.";

/// The choices: what the protocol speaks, and where each came from.
fn choices(p: &ProtocolSummary) -> String {
    let mut out = String::from("**On the wire**\n\n| | | from |\n|---|---|---|\n");
    for (key, choice) in [
        ("format", &p.format),
        ("framing", &p.framing),
        ("handshake", &p.handshake),
    ] {
        let mut value = format!("`{}`", choice.value);
        if !choice.params.is_empty() {
            let params: Vec<String> = choice
                .params
                .iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect();
            value.push_str(&format!(" ({})", params.join(", ")));
        }
        let from = match choice.source {
            Source::Annotation => format!("`@{key}`"),
            Source::PackageDefault if key == "handshake" => "config.idp `handshake`".to_string(),
            Source::PackageDefault => format!("config.idp `wire.{key}`"),
            Source::CoreDefault => "core's default".to_string(),
            // Whichever framing writes its payloads as JSON, by its facts.
            Source::Framing => format!("implied by `{}`", p.framing.value),
        };
        out.push_str(&format!("| {key} | {value} | {from} |\n"));
    }

    // A handshake that takes parameters: every one, as it's in effect.
    let handshake = HandshakeValue::with(
        &p.handshake.value,
        p.handshake
            .params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str())),
    );
    let params = annotations::handshake_params(&handshake.name);
    if !params.is_empty() {
        out.push_str(&format!(
            "\n**`{}`'s parameters**\n\n| parameter | value | |\n|---|---|---|\n",
            handshake.name
        ));
        for param in params {
            let value = handshake.param(param.key).unwrap_or_default();
            let mut notes = vec![];
            if !handshake.params.contains_key(param.key) {
                notes.push("default");
            }
            notes.push(if param.wire {
                "wire: both ends must agree"
            } else {
                "policy: each end applies its own"
            });
            out.push_str(&format!(
                "| `{}` | `{value}` | {} |\n",
                param.key,
                notes.join("; ")
            ));
        }
    }
    // Core's defaults moved (runtime#15): a package released before that and
    // taking them now gets them frozen in, a wire change.
    let takes_new_default = (p.framing.source == Source::CoreDefault
        && p.framing.value == annotations::DEFAULT_FRAMING)
        || (p.handshake.source == Source::CoreDefault
            && p.handshake.value == annotations::DEFAULT_HANDSHAKE);
    if takes_new_default {
        out.push_str(&format!(
            "\n*Core's defaults are `{}` and `{}` since `comline@2`. A package released before then, taking them, \
             takes a breaking bump (minor in 0.x) on its next release. Stating `wire.framing = \"{}\"` keeps the old wire.*\n",
            annotations::DEFAULT_FRAMING,
            annotations::DEFAULT_HANDSHAKE,
            annotations::IMPLICIT_FRAMING,
        ));
    }

    out
}

/// The table of guarantees, and the `@unverified` reason behind it.
fn guarantees(p: &ProtocolSummary) -> String {
    let mut out = String::new();
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
        out.push_str(&format!(
            "| {} | {state} | {} | {accepted} |\n",
            row.guarantee.describe(),
            cell(&why)
        ));
    }
    if let Some(reason) = p.guarantees.iter().find_map(|g| g.reason.as_deref()) {
        out.push_str(&format!("\n`@unverified`: \"{}\"\n", reason));
    }
    out
}

/// `hover` with the summary added, when the cursor at `position` in
/// `source` (the file at `uri`) is on a protocol's name or on its `@framing`
/// or `@handshake`, and the package compiles. Otherwise `hover` unchanged.
///
/// `unsaved` says whether the package's view differs from disk (see
/// [`unsaved_in_package`]); it is asked only when there is a summary to
/// show. When it does, the guarantees are left out.
pub fn extend(
    hover: Option<Hover>,
    source: &str,
    uri: &Url,
    position: Position,
    unsaved: impl FnOnce() -> bool,
) -> Option<Hover> {
    let Some(summary) = (|| {
        let offset = position_to_offset(source, position)?;
        if offset >= source.len() {
            return None;
        }
        let start = source[..offset]
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = source[offset..]
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| offset + i)
            .unwrap_or(source.len());
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
    let summary = MarkedString::from_markdown(if unsaved() {
        markdown_unsaved(&summary)
    } else {
        markdown(&summary)
    });
    let mut parts = match hover {
        None => vec![],
        Some(Hover {
            contents: HoverContents::Array(parts),
            ..
        }) => parts,
        Some(Hover {
            contents: HoverContents::Scalar(part),
            ..
        }) => vec![part],
        Some(Hover {
            contents: HoverContents::Markup(markup),
            ..
        }) => vec![MarkedString::from_markdown(markup.value)],
    };
    parts.push(summary);
    Some(Hover {
        contents: HoverContents::Array(parts),
        range: None,
    })
}

/// The package directory a schema file at `uri` belongs to: the parent of
/// its `src/`. None in the browser, where nothing is on disk.
#[cfg(not(target_arch = "wasm32"))]
fn package_dir(uri: &Url) -> Option<std::path::PathBuf> {
    let path = uri.to_file_path().ok()?;
    Some(
        comline_core::package::layout::schemas_root_for(&path)?
            .parent()?
            .to_path_buf(),
    )
}

#[cfg(target_arch = "wasm32")]
fn package_dir(_uri: &Url) -> Option<std::path::PathBuf> {
    None
}

/// A package's manifest, next to its `src/`.
#[cfg(not(target_arch = "wasm32"))]
const MANIFEST: &str = "config.idp";

/// Whether the package of the schema at `uri` is seen differently by the
/// server than by the saved build: any of `open` (every open document, as
/// its URI and buffer text) that the build reads - a schema of the same
/// package, or its `config.idp` - differs from the file on disk. The hovered
/// document is one of them, and so is an edit in another file of the package.
///
/// Where there is no disk to compare with (the browser, or a `uri` that is
/// no package file), it is conservatively `true`.
#[cfg(not(target_arch = "wasm32"))]
pub fn unsaved_in_package<'a>(
    uri: &Url,
    open: impl IntoIterator<Item = (&'a Url, &'a str)>,
) -> bool {
    let Some(dir) = package_dir(uri) else {
        return true;
    };
    let src = dir.join(comline_core::package::layout::SCHEMAS_DIR);
    let manifest = dir.join(MANIFEST);
    open.into_iter().any(|(url, text)| {
        let Ok(path) = url.to_file_path() else {
            return false;
        };
        let read = same_path(&path, &manifest)
            || (path.extension().is_some_and(|e| e == "ids")
                && comline_core::package::layout::schemas_root_for(&path)
                    .is_some_and(|root| same_path(&root, &src)));
        read && differs_from_disk(&path, text)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn unsaved_in_package<'a>(
    _uri: &Url,
    _open: impl IntoIterator<Item = (&'a Url, &'a str)>,
) -> bool {
    true
}

/// Whether the buffer `text` is not what is saved at `path`: a file that
/// can't be read (never saved, or gone) differs. A trailing newline or CRLF
/// line endings alone are not a difference - nothing the build reads.
#[cfg(not(target_arch = "wasm32"))]
fn differs_from_disk(path: &Path, text: &str) -> bool {
    let normal = |s: &str| s.replace("\r\n", "\n").trim_end_matches('\n').to_string();
    match std::fs::read_to_string(path) {
        Ok(saved) => normal(&saved) != normal(text),
        Err(_) => true,
    }
}

/// The same file, however the two spell it (Windows is case-blind).
#[cfg(not(target_arch = "wasm32"))]
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
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
        let start = SCHEMA[..offset]
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = SCHEMA[offset..]
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| offset + i)
            .unwrap();
        let annotation_key = start > 0 && SCHEMA.as_bytes()[start - 1] == b'@';
        target(
            &document,
            SCHEMA,
            offset,
            &SCHEMA[start..end],
            annotation_key,
        )
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
        assert_eq!(
            target_at(at("Legacy", 1)),
            None,
            "a function named like its protocol"
        );
    }

    #[test]
    fn the_summary_is_a_table_of_choices_and_guarantees() {
        let root = std::env::temp_dir().join(format!(
            "comline-lsp-protocol-summary-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        package(
            &root,
            "wire = { framing = \"datagram@1\" }\n\
             settings = { annotations = { unverified = { allowed = true } framing = { allowed = true } } }\n",
            SCHEMA,
        );
        let orders = summary(&root, "api", "Orders").unwrap();
        assert!(
            markdown(&orders).contains("| framing | `datagram` | config.idp `wire.framing` |"),
            "{}",
            markdown(&orders)
        );

        let legacy = summary(&root, "api", "Legacy").unwrap();
        assert_eq!(
            markdown(&legacy),
            "**On the wire**\n\n| | | from |\n|---|---|---|\n\
             | format | `json@1` | implied by `jsonrpc` |\n\
             | framing | `jsonrpc` (method_names = qualified) | `@framing` |\n\
             | handshake | `none` | `@handshake` |\n\
             \n**Guarantees**\n\n| guarantee | | why | accepted |\n|---|---|---|---|\n\
             | schema agreement | **given up** | handshake `none` | by each consumer |\n\
             | encoding agreement | **given up** | handshake `none` | by each consumer |\n\
             | encoding conformance | held |  |  |\n\
             | version honesty | held |  |  |\n\
             | bounded size | held |  |  |\n\
             \n`@unverified`: \"the 2009 gateway\"\n"
        );
        assert_eq!(summary(&root, "api", "Missing"), None);

        // Through the hover: on `@handshake`, after what the hover had.
        let uri = Url::from_file_path(root.join("src/api.ids")).unwrap();
        let offset = at("handshake", 0);
        let line = SCHEMA[..offset].matches('\n').count() as u32;
        let character = (offset - SCHEMA[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
        let before = Hover {
            contents: HoverContents::Array(vec![MarkedString::from_markdown(
                "`@handshake`".into(),
            )]),
            range: None,
        };
        let Some(Hover {
            contents: HoverContents::Array(parts),
            ..
        }) = extend(
            Some(before),
            SCHEMA,
            &uri,
            Position { line, character },
            || false,
        )
        else {
            panic!("a hover")
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], MarkedString::from_markdown(markdown(&legacy)));
        // Elsewhere, the hover is what it was.
        assert_eq!(
            extend(
                None,
                SCHEMA,
                &uri,
                Position {
                    line: 1,
                    character: 14
                },
                || false,
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn core_defaults_show_comline2_and_its_parameters() {
        let root = std::env::temp_dir().join(format!(
            "comline-lsp-protocol-defaults-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        package(&root, "", "protocol Plain {\n    function ping();\n}\n");
        let plain = markdown(&summary(&root, "api", "Plain").unwrap());
        for row in [
            "| format | `packed@1` | core's default |",
            "| framing | `datagram@3` | core's default |",
            "| handshake | `comline@2` | core's default |",
            "| `scope` | `protocol` | default; policy: each end applies its own |",
            "| `newer_consumer` | `degrade` | default; policy: each end applies its own |",
            "| `same_version_differs` | `refuse` | default; policy: each end applies its own |",
            "| `exchange` | `offer` | default; wire: both ends must agree |",
            "takes a breaking bump (minor in 0.x) on its next release",
        ] {
            assert!(plain.contains(row), "{row}\n{plain}");
        }

        // Parameters written in config.idp; the old framing stated keeps
        // the implicit pair and no note.
        package(
            &root,
            "handshake = { name = \"comline@2\" exchange = \"symmetric\" }\n",
            "protocol Plain {\n    function ping();\n}\n",
        );
        let written = markdown(&summary(&root, "api", "Plain").unwrap());
        assert!(
            written.contains(
                "| handshake | `comline@2` (exchange = symmetric) | config.idp `handshake` |"
            ),
            "{written}"
        );
        assert!(
            written.contains("| `exchange` | `symmetric` | wire: both ends must agree |"),
            "{written}"
        );
        assert!(
            written.contains("| `scope` | `protocol` | default;"),
            "{written}"
        );

        package(
            &root,
            "wire = { framing = \"datagram@2\" }\n",
            "protocol Plain {\n    function ping();\n}\n",
        );
        let old = markdown(&summary(&root, "api", "Plain").unwrap());
        assert!(
            old.contains("| framing | `datagram@2` | config.idp `wire.framing` |"),
            "{old}"
        );
        assert!(
            old.contains("| handshake | `comline@1` | core's default |"),
            "{old}"
        );
        assert!(
            !old.contains("parameters") && !old.contains("breaking bump"),
            "{old}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A package on disk with `schema` as `src/api.ids`, a second schema
    /// `src/other.ids`, and the hover of `needle` in `text` (the buffer of
    /// `api.ids`) with `others` open besides, as the server asks for it:
    /// the hover's parts after what it had, and whether it showed the
    /// guarantees.
    fn hover_with_open(root: &Path, text: &str, others: &[(&str, &str)], needle: &str) -> String {
        let uri = Url::from_file_path(root.join("src/api.ids")).unwrap();
        let mut open = vec![(uri.clone(), text.to_string())];
        for (file, buffer) in others {
            open.push((
                Url::from_file_path(root.join(file)).unwrap(),
                buffer.to_string(),
            ));
        }
        let offset = text.find(needle).unwrap() + 1;
        let line = text[..offset].matches('\n').count() as u32;
        let character = (offset - text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0)) as u32;
        let hover = extend(None, text, &uri, Position { line, character }, || {
            unsaved_in_package(&uri, open.iter().map(|(u, t)| (u, t.as_str())))
        });
        let Some(Hover {
            contents: HoverContents::Array(parts),
            ..
        }) = hover
        else {
            panic!("a hover")
        };
        assert_eq!(parts.len(), 1);
        let MarkedString::String(markdown) = &parts[0] else {
            panic!("markdown")
        };
        markdown.clone()
    }

    const ORDERS: &str = "protocol Orders {\n    function place(id: u64) -> bool;\n}\n";
    const OTHER: &str = "protocol Other {\n    function ping();\n}\n";

    fn stale_package(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "comline-lsp-protocol-stale-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        package(&root, "", ORDERS);
        std::fs::write(root.join("src/other.ids"), OTHER).unwrap();
        root
    }

    fn shows_guarantees(hover: &str) -> bool {
        hover.contains("**Guarantees**") && hover.contains("| schema agreement |")
    }

    fn says_save_first(hover: &str) -> bool {
        hover.contains(&format!("*{UNSAVED_NOTE}*"))
            && !hover.contains("held")
            && !hover.contains("given up")
            && !hover.contains("| schema agreement |")
            && !hover.contains("`@unverified`")
    }

    #[test]
    fn a_clean_buffer_shows_the_guarantees() {
        let root = stale_package("clean");
        let hover = hover_with_open(&root, ORDERS, &[], "Orders");
        assert!(shows_guarantees(&hover), "{hover}");
        assert!(!hover.contains("save to see them"), "{hover}");
        // Others open, and saved as they are on disk.
        let hover = hover_with_open(
            &root,
            ORDERS,
            &[
                ("src/other.ids", OTHER),
                (
                    "config.idp",
                    &std::fs::read_to_string(root.join("config.idp")).unwrap(),
                ),
            ],
            "Orders",
        );
        assert!(shows_guarantees(&hover), "{hover}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unsaved_edit_in_the_hovered_file_hides_the_guarantees() {
        let root = stale_package("hovered");
        // A handshake written in the buffer, the saved build doesn't have it.
        let edited = format!("@handshake = \"none\"\n{ORDERS}");
        let hover = hover_with_open(&root, &edited, &[], "Orders");
        assert!(says_save_first(&hover), "{hover}");
        // What is the buffer's own and what the build resolved stay.
        assert!(hover.starts_with("**On the wire**"), "{hover}");
        assert!(hover.contains("| format |"), "{hover}");

        // And a removed `@unverified`: the build has it, the buffer not.
        let root_unverified = stale_package("removed");
        package(
            &root_unverified,
            "settings = { annotations = { unverified = { allowed = true } } }\n",
            "@handshake = \"none\"\n@unverified = \"the 2009 gateway\"\nprotocol Orders {\n    function place(id: u64) -> bool;\n}\n",
        );
        let saved = hover_with_open(
            &root_unverified,
            &std::fs::read_to_string(root_unverified.join("src/api.ids")).unwrap(),
            &[],
            "Orders",
        );
        assert!(
            saved.contains("`@unverified`: \"the 2009 gateway\""),
            "{saved}"
        );
        let without =
            "@handshake = \"none\"\nprotocol Orders {\n    function place(id: u64) -> bool;\n}\n";
        let hover = hover_with_open(&root_unverified, without, &[], "Orders");
        assert!(says_save_first(&hover), "{hover}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root_unverified);
    }

    #[test]
    fn an_unsaved_edit_elsewhere_in_the_package_hides_the_guarantees() {
        let root = stale_package("elsewhere");
        let manifest = std::fs::read_to_string(root.join("config.idp")).unwrap();

        // Another schema of the package.
        let other = format!("{OTHER}\n// edited\n");
        let hover = hover_with_open(&root, ORDERS, &[("src/other.ids", &other)], "Orders");
        assert!(says_save_first(&hover), "{hover}");

        // The package's `config.idp`: its `wire` and `handshake` defaults.
        let edited = format!("{manifest}handshake = \"none\"\n");
        let hover = hover_with_open(&root, ORDERS, &[("config.idp", &edited)], "Orders");
        assert!(says_save_first(&hover), "{hover}");

        // A file the build doesn't read, or of another package, doesn't.
        let elsewhere = std::env::temp_dir().join(format!(
            "comline-lsp-protocol-stale-else-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&elsewhere);
        package(&elsewhere, "", ORDERS);
        let uri = Url::from_file_path(root.join("src/api.ids")).unwrap();
        let foreign = Url::from_file_path(elsewhere.join("src/api.ids")).unwrap();
        let notes = Url::from_file_path(root.join("notes.txt")).unwrap();
        assert!(!unsaved_in_package(
            &uri,
            [
                (&uri, ORDERS),
                (&foreign, "protocol Changed {}\n"),
                (&notes, "never saved")
            ]
        ));

        // A buffer never saved is not on disk: it differs.
        let fresh = Url::from_file_path(root.join("src/new.ids")).unwrap();
        assert!(unsaved_in_package(&uri, [(&uri, ORDERS), (&fresh, OTHER)]));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn line_endings_and_a_trailing_newline_are_not_unsaved_changes() {
        let root = stale_package("endings");
        let crlf = ORDERS.replace('\n', "\r\n");
        let hover = hover_with_open(&root, &crlf, &[], "Orders");
        assert!(shows_guarantees(&hover), "{hover}");
        let no_newline = ORDERS.trim_end();
        let hover = hover_with_open(&root, no_newline, &[], "Orders");
        assert!(shows_guarantees(&hover), "{hover}");
        let more_newlines = format!("{ORDERS}\n\n");
        let hover = hover_with_open(&root, &more_newlines, &[], "Orders");
        assert!(shows_guarantees(&hover), "{hover}");
        // Any other whitespace is one.
        let indented = ORDERS.replace("    function", "  function");
        let hover = hover_with_open(&root, &indented, &[], "Orders");
        assert!(says_save_first(&hover), "{hover}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
