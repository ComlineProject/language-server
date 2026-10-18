//! Completion inside a `@validators = [...]` list. Between the entries,
//! validator names:
//! - the ones this file declares or a `use` here imports;
//! - then every other validator the package and its dependencies (std
//!   included) declare, which inserts the missing `use` along with the name.
//!
//! Inside an entry's parentheses, `StringBounds(`, the properties of that
//! validator not written yet, but only for one in scope: a validator the
//! file doesn't import has nothing to complete.
//!
//! The file being edited rarely parses mid-entry (`[StringBounds(` is not a
//! schema), so its own validators are read declaration by declaration, the
//! way [`super::uses_in`] reads its `use`s.

use comline_core::schema::idl::grammar::{Declaration, Validator};
use comline_core::schema::ir::frozen::types::TypeRef;
use lsp_types::{CompletionItem, CompletionItemKind, InsertTextFormat, Url};
use std::collections::BTreeSet;

use super::{origin, uses_in};
use crate::analysis::import_check::{self, declares_validator, Target};
use crate::analysis::imports::add_use_edit;
use crate::analysis::project::{Project, ProjectDoc};
use crate::analysis::source::ProjectSource;
use crate::parser;

/// Validator names for an entry of the list.
pub(super) fn names<S: ProjectSource>(source: &str, uri: &Url, other_files: &[S]) -> Vec<CompletionItem> {
    let siblings = Project::new(other_files.iter());
    let mut items = Vec::new();
    // Names in scope here, so nothing is offered twice.
    let mut in_scope: BTreeSet<String> = BTreeSet::new();

    for validator in local_validators(source) {
        if in_scope.insert(validator.name()) {
            items.push(item(&validator, validator.name(), "declared here".to_string()));
        }
    }

    // (file, declared name) already imported, under whatever name.
    let mut imported: BTreeSet<(usize, String)> = BTreeSet::new();
    for (sibling, label, real) in imported_validators(&siblings, source, uri) {
        imported.insert((sibling, real.clone()));
        let doc = &siblings.docs[sibling];
        let Some(validator) = declared(doc, &real) else { continue };
        if in_scope.insert(label.clone()) {
            let detail = match label == real {
                true => format!("from {}", origin(doc)),
                false => format!("`{real}` from {}", origin(doc)),
            };
            items.push(item(validator, label, detail));
        }
    }

    for (index, doc) in siblings.docs.iter().enumerate() {
        for validator in validators_of(doc) {
            let name = validator.name();
            if in_scope.contains(&name) || imported.contains(&(index, name.clone())) {
                continue;
            }
            let path = format!("{}::{}", doc.namespace.join("::"), name);
            let mut item = item(validator, name.clone(), format!("from {} — adds `use {path}`", origin(doc)));
            // After everything in scope; a dependency's after the package's own.
            item.sort_text = Some(match doc.dependency {
                Some(_) => format!("~~{name}"),
                None => format!("~{name}"),
            });
            item.additional_text_edits = Some(vec![add_use_edit(source, &path)]);
            items.push(item);
        }
    }

    items
}

/// The properties of `validator` - as written in the list, so possibly an
/// alias - that `written` doesn't name yet.
pub(super) fn arguments<S: ProjectSource>(
    source: &str,
    uri: &Url,
    other_files: &[S],
    validator: &str,
    written: &[String],
) -> Vec<CompletionItem> {
    let siblings = Project::new(other_files.iter());
    let local = local_validators(source);
    let found = local.iter().find(|v| v.name() == validator).or_else(|| {
        imported_validators(&siblings, source, uri)
            .into_iter()
            .find(|(_, label, _)| label == validator)
            .and_then(|(sibling, _, real)| declared(&siblings.docs[sibling], &real))
    });
    let Some(found) = found else {
        return Vec::new();
    };

    found
        .properties()
        .iter()
        .filter(|p| !written.contains(&p.name()))
        .map(|p| {
            let ty = TypeRef::from_grammar(p.property_type());
            let detail = match p.default_value() {
                Some(default) => format!("{ty} = {}", default.as_text()),
                None => format!("{ty}, required"),
            };
            CompletionItem {
                label: p.name(),
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(detail),
                documentation: p.docstring().map(lsp_types::Documentation::String),
                insert_text: Some(format!("{} = ", p.name())),
                // Required ones first.
                sort_text: Some(format!("{}{}", u8::from(p.default_value().is_some()), p.name())),
                ..Default::default()
            }
        })
        .collect()
}

/// Validators a `use` here imports, as (file, name in scope here, declared
/// name). A glob or whole-namespace `use` brings in no validator - core only
/// expands those to types, protocols and consts.
fn imported_validators(siblings: &Project, source: &str, uri: &Url) -> Vec<(usize, String, String)> {
    let mut found = Vec::new();
    for use_decl in uses_in(source, uri) {
        let Target::Found(sibling, remaining) = import_check::target_of(siblings, None, &use_decl) else {
            continue; // can't list what a file outside the project declares
        };
        let doc = &siblings.docs[sibling];
        let symbols = &use_decl.resolved.symbols;
        if symbols == &["*"] {
            continue;
        }
        if !symbols.is_empty() {
            for name in symbols.iter().filter(|n| declares_validator(doc, n)) {
                found.push((sibling, name.clone(), name.clone()));
            }
        } else if !remaining.is_empty() {
            let real = remaining.join("::");
            if declares_validator(doc, &real) {
                found.push((sibling, use_decl.alias.clone().unwrap_or_else(|| real.clone()), real));
            }
        }
    }
    found
}

/// Every validator this file declares, each `validator ... { ... }` parsed on
/// its own, so the rest of the file needn't parse.
fn local_validators(source: &str) -> Vec<Validator> {
    let cleaned = super::strip_comments_and_strings(source);
    let mut found = Vec::new();
    for (start, _) in cleaned.match_indices("validator") {
        let at_line_start = cleaned[..start].rsplit('\n').next().is_some_and(|before| before.trim().is_empty());
        let word_ends = cleaned[start + "validator".len()..].starts_with(char::is_whitespace);
        if !at_line_start || !word_ends {
            continue;
        }
        let Some(end) = block_end(&cleaned, start) else { continue };
        let Some(document) = parser::parse(&source[start..end]).ok().and_then(|r| r.document) else { continue };
        found.extend(document.0.into_iter().filter_map(|decl| match decl.value {
            Declaration::Validator(v) => Some(v),
            _ => None,
        }));
    }
    found
}

/// The byte just past the `}` closing the first `{` after `start`.
fn block_end(cleaned: &str, start: usize) -> Option<usize> {
    let open = start + cleaned[start..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in cleaned[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn validators_of<'d>(doc: &'d ProjectDoc) -> impl Iterator<Item = &'d Validator> {
    doc.document.0.iter().filter_map(|decl| match &decl.value {
        Declaration::Validator(v) => Some(v),
        _ => None,
    })
}

fn declared<'d>(doc: &'d ProjectDoc, name: &str) -> Option<&'d Validator> {
    validators_of(doc).find(|v| v.name() == name)
}

/// One validator as an entry: its name, plus a snippet for the arguments
/// it can't do without - none, and it goes in bare.
fn item(validator: &Validator, label: String, detail: String) -> CompletionItem {
    let required: Vec<String> = validator
        .properties()
        .iter()
        .filter(|p| p.default_value().is_none())
        .map(|p| p.name())
        .collect();
    let signature = validator
        .properties()
        .iter()
        .map(|p| format!("{}: {}", p.name(), TypeRef::from_grammar(p.property_type())))
        .collect::<Vec<_>>()
        .join(", ");

    let snippet = (!required.is_empty()).then(|| {
        let args: Vec<String> =
            required.iter().enumerate().map(|(i, name)| format!("{name} = ${{{}}}", i + 1)).collect();
        format!("{label}({})", args.join(", "))
    });

    CompletionItem {
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some(format!("validator {}({signature}) — {detail}", validator.name())),
        documentation: validator.docstring().map(lsp_types::Documentation::String),
        insert_text_format: snippet.as_ref().map(|_| InsertTextFormat::SNIPPET),
        insert_text: snippet,
        label,
        ..Default::default()
    }
}
