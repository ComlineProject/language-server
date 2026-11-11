// Completion handler - provides auto-completion suggestions

mod use_path;
mod validator_body;
mod validators;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use crate::analysis::import_check::{self, declares_type, file_name, is_type_kind, Target};
use crate::analysis::imports::{self, add_use_edit, ResolvedUse};
use crate::analysis::project::{Project, ProjectDoc};
use crate::analysis::source::ProjectSource;
use crate::analysis::stdlib;
use crate::analysis::symbols;
use crate::analysis::symbols::Namespace;
use crate::parser;
use crate::util::{in_comment_or_string, position_to_offset};
use comline_core::config_value::{self, ValueType};
use comline_core::schema::idl::annotations::{self, AnnotationScope};
use comline_core::schema::idl::vocabulary::{self, KeywordKind};
use comline_core::settings::keys::{self, NameKind};
use lsp_types::{CompletionItem, CompletionItemKind, Position, SymbolKind, Url};


/// Get completion suggestions at a position, considering only this file.
pub fn get_completions(source: &str, uri: &Url, position: Position) -> Vec<CompletionItem> {
    get_completions_with_project::<(Url, String)>(source, uri, position, &[])
}

/// Get completion suggestions at a position, with `other_files` (every
/// other file in the package, as `(uri, source)` pairs) as the project: in a type
/// position, the types this file's `use`s bring in from them, and then
/// every other type they declare - picking one of those also adds its
/// `use` line.
pub fn get_completions_with_project<S: ProjectSource>(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[S],
) -> Vec<CompletionItem> {
    mark_deprecated_words(
        completions_at(source, uri, position, other_files),
        vocabulary::deprecation,
    )
}

/// `items` with every keyword or primitive type an edition deprecates
/// (core#102) tagged deprecated, which the editor shows struck through.
/// Still offered: a package on an older edition may use it freely.
/// `deprecation` is [`vocabulary::deprecation`] outside tests.
fn mark_deprecated_words(
    mut items: Vec<CompletionItem>,
    deprecation: impl Fn(&str) -> Option<&'static vocabulary::Deprecation>,
) -> Vec<CompletionItem> {
    for item in &mut items {
        let a_word = matches!(
            item.kind,
            Some(CompletionItemKind::KEYWORD | CompletionItemKind::TYPE_PARAMETER)
        );
        if !a_word {
            continue;
        }
        if let Some(d) = deprecation(&item.label) {
            let tags = item.tags.get_or_insert_with(Vec::new);
            if !tags.contains(&lsp_types::CompletionItemTag::DEPRECATED) {
                tags.push(lsp_types::CompletionItemTag::DEPRECATED);
            }
            let note = format!("deprecated since edition {} — {}", d.since, d.note);
            item.detail = Some(match item.detail.take() {
                Some(detail) => format!("{detail} ({note})"),
                None => note,
            });
        }
    }
    items
}

fn completions_at<S: ProjectSource>(
    source: &str,
    uri: &Url,
    position: Position,
    other_files: &[S],
) -> Vec<CompletionItem> {
    let offset = match position_to_offset(source, position) {
        Some(o) => o,
        None => return get_keyword_completions(),
    };

    // No code completion inside a `//` comment (docstrings included) or a
    // string literal — the text there isn't Comline.
    if in_comment_or_string(source, offset) {
        return Vec::new();
    }

    // In a `use` path, only what the path can go on with.
    if let Some(context) = use_path::context_at(source, offset) {
        return match context {
            use_path::UseContext::Path(prefix) => {
                use_path::completions(&prefix, source, uri, offset, other_files)
            }
            use_path::UseContext::Nothing => Vec::new(),
        };
    }

    // Best-effort: a symbol table of user-declared struct/enum/type-alias
    // names, when the document happens to fully parse right now. While
    // actively typing it almost never will (the whole document is one
    // grammar rule, and a single in-progress declaration fails the entire
    // parse) — context detection below works directly on the raw text
    // either way, so a mid-edit document still gets real, context-aware
    // suggestions; this only widens the *type* list with names that
    // haven't been reparsed yet.
    let symbol_table = parser::parse(source)
        .ok()
        .and_then(|r| r.document)
        .map(|document| symbols::build_symbol_table(&document, uri, source));

    let mut completions = Vec::new();

    match determine_context(source, offset) {
        CompletionContext::TypePosition => {
            // Right after `:` / `->`, or inside an unclosed `union(...)` —
            // every known type (primitives first, then user-declared
            // struct/enum/alias names) is relevant here, regardless of how
            // much whitespace or partial typing separates the cursor from
            // that `:`/`->`/`(`.
            completions.extend(get_primitive_type_completions());
            completions.extend(get_type_completions(symbol_table.as_ref(), source));
            completions.extend(get_import_completions(
                source,
                uri,
                other_files,
                symbol_table.as_ref(),
            ));
        }
        CompletionContext::TopLevel => {
            // Nothing (or only a partial keyword) typed yet, no enclosing
            // block — only a new top-level declaration can start here.
            completions.extend(get_keyword_completions());
        }
        CompletionContext::StructBody => {
            // Start of a new struct/error field, before its name —
            // nothing to suggest but the one modifier keyword a field can
            // carry (the field's own name is an arbitrary new identifier,
            // and its type is handled by `TypePosition` once `:` appears).
            completions.push(optional_keyword_completion());
        }
        CompletionContext::ProtocolBody => {
            // Start of a new protocol member — today that can only be a
            // `function`.
            completions.push(function_keyword_completion());
        }
        CompletionContext::AnnotationKey(scope) => {
            completions.extend(get_annotation_completions(
                scope,
                &effective_settings(source, offset, other_files),
            ));
        }
        CompletionContext::ValidatorBody(body) => match validator_body::completions(body) {
            Some(items) => completions.extend(items),
            // A property's type: as after any other `:`.
            None => {
                completions.extend(get_primitive_type_completions());
                completions.extend(get_type_completions(symbol_table.as_ref(), source));
                completions.extend(get_import_completions(
                    source,
                    uri,
                    other_files,
                    symbol_table.as_ref(),
                ));
            }
        },
        CompletionContext::ValidatorFor => {
            completions.push(for_keyword_completion());
        }
        CompletionContext::AfterValidatorTarget => {
            // A `,` and another target, or the body's `{`: punctuation,
            // nothing to complete.
        }
        CompletionContext::DeclarationName => {
            // Naming something new — the identifier right after `struct`/
            // `enum`/.../`function`, or an enum's own variant name. It's
            // an arbitrary new name, not a reference to anything that
            // already exists, so there is nothing useful to suggest.
        }
        CompletionContext::SettingsBody => {
            completions.extend(get_settings_completions(source, offset));
        }
        CompletionContext::ValidatorName => {
            completions.extend(validators::names(source, uri, other_files));
        }
        CompletionContext::ValidatorArgument { validator, written } => {
            completions.extend(validators::arguments(
                source,
                uri,
                other_files,
                &validator,
                &written,
            ));
        }
        CompletionContext::Unknown => {
            // Context couldn't be narrowed down — offer the same broad
            // mix completion has always fallen back to, rather than
            // nothing.
            completions.extend(get_keyword_completions());
            completions.extend(get_primitive_type_completions());
            completions.extend(get_type_completions(symbol_table.as_ref(), source));
            completions.extend(get_import_completions(
                source,
                uri,
                other_files,
                symbol_table.as_ref(),
            ));
        }
    }

    completions
}

/// Type names from the package's other files. First the ones this file's `use`s
/// bring into scope - under their alias, if they have one - then every
/// other type those files declare, sorted after everything else, which
/// inserts the missing `use` line along with the name. Names already
/// declared here, or already in scope, aren't offered again.
fn get_import_completions<S: ProjectSource>(
    source: &str,
    uri: &Url,
    other_files: &[S],
    symbol_table: Option<&symbols::SymbolTable>,
) -> Vec<CompletionItem> {
    let siblings = Project::new(other_files.iter());
    let mut in_scope: BTreeSet<String> = symbol_table
        .map(|t| t.all_symbols().iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    // (file, declared name) already imported, under whatever name.
    let mut imported: BTreeSet<(usize, String)> = BTreeSet::new();
    let mut items = Vec::new();

    for use_decl in uses_in(source, uri) {
        let Target::Found(sibling, remaining) = import_check::target_of(&siblings, None, &use_decl)
        else {
            continue; // can't list what a file outside the project declares
        };
        let doc = &siblings.docs[sibling];
        let symbols = &use_decl.resolved.symbols;

        // (name in scope here, name declared there)
        let names: Vec<(String, String)> =
            if symbols == &["*"] || (symbols.is_empty() && remaining.is_empty()) {
                type_names(doc).map(|n| (n.clone(), n)).collect()
            } else if !symbols.is_empty() {
                symbols
                    .iter()
                    .filter(|n| declares_type(doc, n))
                    .map(|n| (n.clone(), n.clone()))
                    .collect()
            } else {
                let real = remaining.join("::");
                match declares_type(doc, &real) {
                    true => vec![(use_decl.alias.clone().unwrap_or_else(|| real.clone()), real)],
                    false => vec![],
                }
            };

        for (label, real) in names {
            imported.insert((sibling, real.clone()));
            if in_scope.insert(label.clone()) {
                let detail = match label == real {
                    true => format!("from {}", origin(doc)),
                    false => format!("`{real}` from {}", origin(doc)),
                };
                items.push(type_item(doc, &real, label, detail));
            }
        }
    }

    for (index, doc) in siblings.docs.iter().enumerate() {
        for name in type_names(doc) {
            if in_scope.contains(&name) || imported.contains(&(index, name.clone())) {
                continue;
            }
            let path = format!("{}::{}", doc.namespace.join("::"), name);
            let mut item = type_item(
                doc,
                &name,
                name.clone(),
                format!("from {} — adds `use {path}`", origin(doc)),
            );
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

/// The `use`/`import` declarations in `source`, each parsed on its own
/// line, so they're there even while the rest of the file doesn't parse.
fn uses_in(source: &str, uri: &Url) -> Vec<ResolvedUse> {
    let namespace = imports::namespace_of(uri);
    source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("use ") || line.starts_with("import "))
        .filter_map(|line| parser::parse(line).ok()?.document)
        .flat_map(|document| imports::resolved_imports(&document, &namespace))
        .collect()
}

/// Where a type comes from, for an item's detail: `` `types.ids` ``, or
/// `` `models.ids` (dependency `shared_types`) ``.
fn origin(doc: &ProjectDoc) -> String {
    match doc.dependency {
        Some(dependency) => format!("`{}` ({})", file_name(doc), stdlib::owner(dependency)),
        None => format!("`{}`", file_name(doc)),
    }
}

/// Every struct, enum and type alias `doc` declares.
fn type_names<'d>(doc: &'d ProjectDoc) -> impl Iterator<Item = String> + 'd {
    doc.symbols
        .all_symbols()
        .iter()
        .filter(|s| is_type_kind(s.kind))
        .map(|s| s.name.clone())
}

fn type_item(doc: &ProjectDoc, real: &str, label: String, detail: String) -> CompletionItem {
    let kind = match doc
        .symbols
        .get_in(real, Namespace::Declaration)
        .map(|s| s.kind)
    {
        Some(SymbolKind::ENUM) => CompletionItemKind::ENUM,
        Some(SymbolKind::TYPE_PARAMETER) => CompletionItemKind::CLASS,
        _ => CompletionItemKind::STRUCT,
    };
    CompletionItem {
        label,
        kind: Some(kind),
        detail: Some(detail),
        ..Default::default()
    }
}

/// Blank out `//` line comments, `/* ... */` block comments, and the
/// *contents* of `"…"` string literals — replacing each covered character
/// with a space, preserving length and every newline — so a backward token
/// scan never gets confused by punctuation that only exists inside a
/// comment or a string (an `=` inside a docstring, a stray `{` inside a
/// commented-out declaration, …). Strings don't span lines, but a block
/// comment can, so `in_block_comment` carries across the per-line loop
/// below the same way it does in `semantic_tokens::lex_line`. Operates on
/// `char`s throughout (not raw bytes), since a docstring may contain
/// arbitrary UTF-8.
fn strip_comments_and_strings(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_block_comment = false;
    for line in source.split_inclusive('\n') {
        let mut chars = line.char_indices().peekable();
        let mut in_str = false;
        while let Some((_, c)) = chars.next() {
            if in_block_comment {
                if c == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                    if c == '*' && chars.peek().map(|&(_, c2)| c2) == Some('/') {
                        chars.next();
                        out.push(' ');
                        in_block_comment = false;
                    }
                }
                continue;
            }
            if in_str {
                if c == '\\' {
                    out.push(' ');
                    if let Some(&(_, next)) = chars.peek() {
                        if next != '\n' {
                            chars.next();
                            out.push(' ');
                        }
                    }
                    continue;
                }
                if c == '"' {
                    in_str = false;
                }
                out.push(' ');
                continue;
            }
            if c == '"' {
                in_str = true;
                out.push(' ');
                continue;
            }
            if c == '/' && chars.peek().map(|&(_, c2)| c2) == Some('/') {
                chars.next();
                out.push(' ');
                out.push(' ');
                for (_, c2) in chars.by_ref() {
                    out.push(if c2 == '\n' { '\n' } else { ' ' });
                }
                break;
            }
            if c == '/' && chars.peek().map(|&(_, c2)| c2) == Some('*') {
                chars.next();
                out.push(' ');
                out.push(' ');
                in_block_comment = true;
                continue;
            }
            out.push(c);
        }
    }
    out
}

/// One token in a backward scan from the cursor — just enough shape to
/// classify completion context: identifier/keyword runs, and the specific
/// punctuation marks the grammar uses around a type (`:` for a field/arg/
/// const type, `->` for a return type, parens for `union(...)` and a
/// function's argument list, braces for every block).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    Punct(char),
    Arrow,
}

/// Tokenize `cleaned[..offset]` (forward is simplest; callers only ever
/// look at the last few entries). `cleaned` should already be the output of
/// [`strip_comments_and_strings`] — this doesn't know about comments or
/// strings itself. Each token carries the byte offset its text *ends* at,
/// so a caller can tell whether it directly abuts `offset` (still being
/// typed) or has trailing whitespace after it (already finished).
fn tokenize_prefix(cleaned: &str, offset: usize) -> Vec<(Tok<'_>, usize)> {
    let end = offset.min(cleaned.len());
    let text = &cleaned[..end];
    let mut toks = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            let mut last_end = i + c.len_utf8();
            while let Some(&(j, c2)) = chars.peek() {
                if c2.is_alphanumeric() || c2 == '_' {
                    last_end = j + c2.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            toks.push((Tok::Word(&text[start..last_end]), last_end));
            continue;
        }
        if c == '-' && chars.peek().map(|&(_, c2)| c2) == Some('>') {
            let (j, _) = chars.next().unwrap();
            toks.push((Tok::Arrow, j + 1));
            continue;
        }
        toks.push((Tok::Punct(c), i + c.len_utf8()));
    }

    toks
}

/// Whether `word` introduces a declaration — completion offers nothing
/// while the identifier right after one of these is being typed (see
/// [`CompletionContext::DeclarationName`]). Sourced from
/// `vocabulary::KeywordKind::Declaration`, which also includes `use`/
/// `import`; a `use` path never gets here, `use_path` completes it first.
fn is_declaration_keyword(word: &str) -> bool {
    match vocabulary::keyword(word) {
        // `function` is the one `Member`-kind keyword followed by a
        // user-chosen name (unlike its siblings `validate`/`message`,
        // which aren't) — included explicitly rather than widening
        // `Member` wholesale, which would wrongly swallow completions
        // after those two as well.
        Some(k) => k.kind == KeywordKind::Declaration || word == "function",
        None => false,
    }
}

/// Determine completion context based on position — a backward scan over
/// tokens, not a full grammar parse, so it degrades gracefully on exactly
/// the syntactically-incomplete text that's the norm while actively
/// typing.
fn determine_context(source: &str, offset: usize) -> CompletionContext {
    if offset == 0 {
        return CompletionContext::TopLevel;
    }
    let offset = offset.min(source.len());
    let cleaned = strip_comments_and_strings(source);
    let mut toks = tokenize_prefix(&cleaned, offset);

    // Drop a trailing word that's still being typed (abuts the cursor with
    // no whitespace in between) — the client already filters completions
    // by it; it isn't part of the surrounding context being classified.
    if let Some(&(Tok::Word(_), end)) = toks.last() {
        if end == offset {
            toks.pop();
        }
    }

    // Naming something new: the identifier right after a
    // declaration-introducing keyword.
    if let Some(&(Tok::Word(w), _)) = toks.last() {
        if is_declaration_keyword(w) {
            return CompletionContext::DeclarationName;
        }
    }

    // A validator's header, `validator NAME for TYPE, TYPE {`.
    if let Some(context) = validator_header_context(&toks) {
        return context;
    }

    // Its body: properties and `validate { assert(...) }`.
    if let Some(body) = validator_body::context(&cleaned, offset, &toks) {
        return CompletionContext::ValidatorBody(body);
    }

    // Right after a bare `@` — the start of an annotation key. Which keys
    // make sense depends on what `@key=value` is about to attach to, which
    // the grammar ties to the *enclosing* block, not anything about `@`
    // itself: a struct/error body (a field annotation, e.g. `@validators`),
    // a protocol body (a function annotation, e.g. `@timeout_ms`), or top
    // level (a struct's or protocol's own leading annotation, e.g.
    // `@framing` before `protocol`). `enclosing_block` already answers
    // exactly that question for `StructBody` / `ProtocolBody` — reused
    // as-is; it ignores the trailing `@` token like it does any other
    // non-brace token.
    if matches!(toks.last(), Some((Tok::Punct('@'), _))) {
        match enclosing_block(&toks) {
            Some(EnclosingBlock::Struct) => {
                return CompletionContext::AnnotationKey(AnnotationScope::Field)
            }
            Some(EnclosingBlock::Protocol) => {
                return CompletionContext::AnnotationKey(AnnotationScope::Function)
            }
            // Top level: a struct's, error's or protocol's leading position,
            // or a `type` alias's - the same position, told apart by the
            // declaration already written below, when there is one.
            None if declaration_after(&cleaned, offset) == Some("type") => {
                return CompletionContext::AnnotationKey(AnnotationScope::TypeAlias)
            }
            None => return CompletionContext::AnnotationKey(AnnotationScope::Leading),
            // An enum variant, a settings entry, or some other block don't
            // take annotations — fall through to the same handling
            // `enclosing_block` gives any other token there (nothing for
            // an enum, the broad fallback otherwise), rather than
            // inventing a separate rule just for a stray `@`.
            Some(EnclosingBlock::Enum)
            | Some(EnclosingBlock::Settings)
            | Some(EnclosingBlock::Other) => {}
        }
    }

    // In a `@validators = [...]` list - before `:` and the parentheses
    // below, which mean a type everywhere else.
    if let Some(context) = validator_list_context(&toks) {
        return context;
    }

    // Right after `:` (a field/argument/const/validator-property type) or
    // `->` (a return type).
    match toks.last() {
        Some((Tok::Punct(':'), _)) | Some((Tok::Arrow, _)) => {
            return CompletionContext::TypePosition;
        }
        _ => {}
    }

    // Inside any unclosed `(...)` — a `union(...)`'s member list or a
    // function's argument list (where a bare type, with no `name:`
    // prefix, is also valid grammar) both expect a type here.
    if in_unclosed_paren(&toks) {
        return CompletionContext::TypePosition;
    }

    match enclosing_block(&toks) {
        Some(EnclosingBlock::Struct) => CompletionContext::StructBody,
        Some(EnclosingBlock::Protocol) => CompletionContext::ProtocolBody,
        // An enum variant, like a declaration's own name, is an arbitrary
        // new identifier — nothing to suggest.
        Some(EnclosingBlock::Enum) => CompletionContext::DeclarationName,
        Some(EnclosingBlock::Settings) => CompletionContext::SettingsBody,
        Some(EnclosingBlock::Other) => CompletionContext::Unknown,
        None => CompletionContext::TopLevel,
    }
}

/// What the cursor names inside an unclosed `@validators = [`: an entry's
/// validator, or, inside that entry's `(`, an argument's name -
/// [`CompletionContext::DeclarationName`] (nothing to offer) after an
/// argument's `=`, where its value goes. `None` outside such a list.
fn validator_list_context(toks: &[(Tok, usize)]) -> Option<CompletionContext> {
    let mut parens = 0i32;
    let mut brackets = 0i32;
    // The unclosed `(` the cursor is in, if any.
    let mut open_paren = None;

    for i in (0..toks.len()).rev() {
        match toks[i].0 {
            Tok::Punct(')') => parens += 1,
            Tok::Punct('(') if parens > 0 => parens -= 1,
            // Two unclosed `(`: not a validator call.
            Tok::Punct('(') if open_paren.is_some() => return None,
            Tok::Punct('(') => open_paren = Some(i),
            Tok::Punct(']') => brackets += 1,
            Tok::Punct('[') if brackets > 0 => brackets -= 1,
            Tok::Punct('[') => {
                let listed = i >= 3
                    && toks[i - 3].0 == Tok::Punct('@')
                    && toks[i - 2].0 == Tok::Word("validators")
                    && toks[i - 1].0 == Tok::Punct('=');
                if !listed {
                    return None;
                }
                let Some(open) = open_paren else {
                    return Some(CompletionContext::ValidatorName);
                };
                let Tok::Word(validator) = toks[open - 1].0 else {
                    return None;
                };
                if matches!(toks.last(), Some((Tok::Punct('='), _))) {
                    return Some(CompletionContext::DeclarationName);
                }
                let written = toks[open + 1..]
                    .windows(2)
                    .filter_map(|pair| match (pair[0].0, pair[1].0) {
                        (Tok::Word(name), Tok::Punct('=')) => Some(name.to_string()),
                        _ => None,
                    })
                    .collect();
                return Some(CompletionContext::ValidatorArgument {
                    validator: validator.to_string(),
                    written,
                });
            }
            // Out of the annotation and into a declaration: no list here.
            Tok::Punct('{') | Tok::Punct('}') | Tok::Punct(';') => return None,
            _ => {}
        }
    }
    None
}

/// In a validator's header, `validator NAME for TYPE, TYPE {`, after its
/// name: only `for` can come next, then a target type after `for` and after
/// each `,`. `None` anywhere else, including the name itself.
fn validator_header_context(toks: &[(Tok, usize)]) -> Option<CompletionContext> {
    // The last `validator`, unless a block opened or closed since.
    let start = toks.iter().rposition(|(t, _)| {
        matches!(
            t,
            Tok::Word("validator") | Tok::Punct('{') | Tok::Punct('}') | Tok::Punct(';')
        )
    })?;
    if !matches!(toks[start].0, Tok::Word("validator")) {
        return None;
    }
    match &toks[start + 1..] {
        [(Tok::Word(_), _)] => Some(CompletionContext::ValidatorFor),
        [(Tok::Word(_), _), (Tok::Word("for"), _), rest @ ..] => match rest.last() {
            None | Some((Tok::Punct(','), _)) => Some(CompletionContext::TypePosition),
            Some(_) => Some(CompletionContext::AfterValidatorTarget),
        },
        _ => None,
    }
}

/// Whether the cursor sits inside a `(` that hasn't been closed yet, per a
/// backward-matching scan over `toks`.
fn in_unclosed_paren(toks: &[(Tok, usize)]) -> bool {
    let mut depth = 0i32;
    for (tok, _) in toks.iter().rev() {
        match tok {
            Tok::Punct(')') => depth += 1,
            Tok::Punct('(') => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

enum EnclosingBlock {
    Struct,
    Protocol,
    Enum,
    Settings,
    Other,
}

/// What kind of block the innermost unclosed `{` (if any) belongs to,
/// determined by the declaration keyword immediately before that block's
/// own name. A function's `(...)...;` has no braces of its own, so this
/// never needs to look past one — the protocol's own `{` is always the
/// innermost unclosed brace while positioned between two function
/// declarations.
fn enclosing_block(toks: &[(Tok, usize)]) -> Option<EnclosingBlock> {
    let mut depth = 0i32;
    for i in (0..toks.len()).rev() {
        match toks[i].0 {
            Tok::Punct('}') => depth += 1,
            Tok::Punct('{') => {
                if depth > 0 {
                    depth -= 1;
                    continue;
                }
                // Found the innermost unclosed `{`. Walk back past the
                // block's own name (one Word, if present) to the
                // declaration keyword that introduced it - unless that
                // Word is itself `settings` (the one keyword here whose
                // name is optional: an unnamed `settings { ... }` has
                // nothing between the keyword and `{` to skip past).
                let mut j = i;
                if j > 0 {
                    if let Tok::Word(w) = toks[j - 1].0 {
                        if w != "settings" {
                            j -= 1;
                        }
                    }
                }
                let kw = (j > 0).then(|| toks[j - 1].0).and_then(|t| match t {
                    Tok::Word(w) => Some(w),
                    _ => None,
                });
                return Some(match kw {
                    Some("struct") | Some("error") => EnclosingBlock::Struct,
                    Some("protocol") => EnclosingBlock::Protocol,
                    Some("enum") => EnclosingBlock::Enum,
                    Some("settings") => EnclosingBlock::Settings,
                    _ => EnclosingBlock::Other,
                });
            }
            _ => {}
        }
    }
    None
}

fn optional_keyword_completion() -> CompletionItem {
    CompletionItem {
        label: "optional".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Optional field modifier".to_string()),
        ..Default::default()
    }
}

/// `for`, after a validator's name. Inserted with its space, and the
/// suggestions reopen on the target types.
fn for_keyword_completion() -> CompletionItem {
    CompletionItem {
        label: "for".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: vocabulary::keyword("for").map(|k| k.description.to_string()),
        insert_text: Some("for ".to_string()),
        command: Some(use_path::trigger_suggest()),
        ..Default::default()
    }
}

fn function_keyword_completion() -> CompletionItem {
    CompletionItem {
        label: "function".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        detail: Some("Define a function".to_string()),
        insert_text: Some("function $1($2)$0;".to_string()),
        insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
        ..Default::default()
    }
}

/// The snippet a declaration keyword inserts — completion-specific
/// (editor insertion mechanics), so it lives here rather than in
/// `vocabulary`, which only describes *meaning*. Mirrors
/// `annotation_insert_text`'s split. `None` (just `use`/`import`) falls
/// back to inserting the bare keyword.
fn keyword_snippet(text: &str) -> Option<&'static str> {
    match text {
        "struct" => Some("struct $1 {\n\t$0\n}"),
        "enum" => Some("enum $1 {\n\t$0\n}"),
        "protocol" => Some("protocol $1 {\n\t$0\n}"),
        "const" => Some("const $1: $2 = $0"),
        "type" => Some("type $1 = $0"),
        "error" => Some("error $1 {\n\tmessage = \"$2\"\n\t$0\n}"),
        "settings" => Some("settings $1 {\n\t$0\n}"),
        // `for` is required: a validator names the type(s) it checks.
        "validator" => Some("validator $1 for $2 {\n\t$0\n}"),
        _ => None,
    }
}

/// Keyword completions for a new top-level declaration — every
/// `KeywordKind::Declaration` word, sourced from `vocabulary` so this list
/// can't drift from the grammar (it already has once: `self`/`parent`/
/// `crate`/etc. were invisible here before this existed).
fn get_keyword_completions() -> Vec<CompletionItem> {
    vocabulary::keywords_of_kind(KeywordKind::Declaration)
        .map(|k| {
            let snippet = keyword_snippet(k.text);
            CompletionItem {
                label: k.text.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(k.description.to_string()),
                insert_text: snippet.map(str::to_string),
                insert_text_format: snippet.map(|_| lsp_types::InsertTextFormat::SNIPPET),
                ..Default::default()
            }
        })
        .collect()
}

/// The types `source` declares at top level, found by scanning its tokens:
/// for a document that doesn't parse, which while typing is most of the
/// time. The struct, enum and alias names are still there, and they're what
/// a field, argument or validator target refers to.
fn declared_types_by_scan(source: &str) -> Vec<CompletionItem> {
    let cleaned = strip_comments_and_strings(source);
    let toks = tokenize_prefix(&cleaned, cleaned.len());
    let mut depth = 0usize;
    let mut items = Vec::new();
    for pair in toks.windows(2) {
        match (&pair[0].0, &pair[1].0) {
            (Tok::Punct('{'), _) => depth += 1,
            (Tok::Punct('}'), _) => depth = depth.saturating_sub(1),
            (Tok::Word(keyword), Tok::Word(name)) if depth == 0 => {
                let kind = match *keyword {
                    "struct" => CompletionItemKind::STRUCT,
                    "enum" => CompletionItemKind::ENUM,
                    "type" => CompletionItemKind::CLASS,
                    _ => continue,
                };
                if !items.iter().any(|i: &CompletionItem| i.label == *name) {
                    items.push(CompletionItem {
                        label: name.to_string(),
                        kind: Some(kind),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
    items
}

/// Primitive type completions, sourced from `vocabulary::PRIMITIVES` —
/// this used to hand-list `i8/i16/i32/i64`, which were never real Comline
/// syntax (signed integers are `s`-prefixed).
fn get_primitive_type_completions() -> Vec<CompletionItem> {
    vocabulary::PRIMITIVES
        .iter()
        .map(|p| CompletionItem {
            label: p.name.to_string(),
            kind: Some(CompletionItemKind::TYPE_PARAMETER),
            detail: Some(p.description.to_string()),
            ..Default::default()
        })
        .collect()
}

/// User-defined type completions from the symbol table — struct, enum, and
/// `type` alias names only (not const/protocol names, which aren't valid
/// in a type position). `None` when the document doesn't currently parse —
/// no custom names are available yet, but callers still have the
/// primitive list.
fn get_type_completions(
    symbol_table: Option<&symbols::SymbolTable>,
    source: &str,
) -> Vec<CompletionItem> {
    let Some(symbol_table) = symbol_table else {
        return declared_types_by_scan(source);
    };
    symbol_table
        .all_symbols()
        .iter()
        .filter_map(|symbol| {
            let kind = match symbol.kind {
                lsp_types::SymbolKind::STRUCT => CompletionItemKind::STRUCT,
                lsp_types::SymbolKind::ENUM => CompletionItemKind::ENUM,
                lsp_types::SymbolKind::TYPE_PARAMETER => CompletionItemKind::CLASS,
                _ => return None,
            };

            let detail = if !symbol.children.is_empty() {
                Some(format!("{} - {} items", symbol.name, symbol.children.len()))
            } else {
                None
            };

            Some(CompletionItem {
                label: symbol.name.clone(),
                kind: Some(kind),
                detail,
                ..Default::default()
            })
        })
        .collect()
}

/// The snippet for one `= value` tab-stop of the given type, numbered
/// `n`. Insertion mechanics are the editor's business; *what values are
/// legal* is core's, so the shape comes from [`ValueType`] rather than
/// from a table here that would have to be kept in step with it by hand.
///
/// Returns `None` for a type that takes no value at all, so the caller
/// inserts a bare marker rather than `key = ` with nothing after it.
fn value_snippet(ty: &ValueType, n: usize) -> Option<String> {
    let spellings = config_value::spellings(ty);
    Some(match ty {
        ValueType::Marker => return None,
        ValueType::Bool | ValueType::BoolOrStr => format!("${{{n}:true}}"),
        ValueType::Integer => format!("${{{n}:0}}"),
        ValueType::Str => format!("\"${{{n}:text}}\""),
        // A closed set the editor can offer as a choice list. Quoted for
        // `StrEnum` (a string from a set), bare for `Keyword`.
        ValueType::StrEnum(_) => format!("\"${{{n}|{}|}}\"", spellings.join(",")),
        // A single legal keyword is not a choice - insert it finished.
        ValueType::Keyword(_) if spellings.len() == 1 => spellings[0].to_string(),
        ValueType::Keyword(_) => format!("${{{n}|{}|}}", spellings.join(",")),
        ValueType::Name => format!("${{{n}:Name}}"),
        ValueType::ValidatorList => format!("[${n}]"),
        // One of several forms: the first, the plainest (`@framing`'s name,
        // not its dictionary).
        ValueType::AnyOf(types) => return types.first().and_then(|first| value_snippet(first, n)),
        // The keys it can't do without, each its own tab-stop.
        ValueType::Dict(keys) => {
            let entries: Vec<String> = keys
                .iter()
                .filter(|k| k.required)
                .enumerate()
                .map(|(i, k)| match value_snippet(&k.value, n + i) {
                    Some(value) => format!("{} = {value}", k.key),
                    None => k.key.to_string(),
                })
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
    })
}

/// The snippet a known annotation key inserts. The value half comes from
/// the annotation's own [`ValueType`] in core, so a key whose accepted
/// values change does not need a matching edit here - this used to carry
/// its own copy of `framing`'s choice list, and listed only the two
/// canonical spellings while core accepted five.
///
/// `None` inserts just the bare key name, which is what a marker
/// annotation like `idempotent` should produce.
fn annotation_insert_text(info: &annotations::AnnotationInfo) -> Option<String> {
    let value = value_snippet(&info.value, 1)?;
    Some(format!("{} = {value}$0", info.key))
}

/// Each placeholder segment replaced with a snippet tab-stop, plus a
/// trailing ` = <value>` tab-stop whose shape comes from the key's own
/// [`ValueType`] — a completion inserts a whole, immediately-editable
/// entry, not just the key. `segments` is already scoped to whatever's
/// left to type (see [`matching_remaining`]) — asking at the very start of
/// a block passes a catalog entry's full path, same as it always has.
///
/// Two things used to be hardcoded here. The value was always
/// `${n:true}`, which is wrong for `diagnostics.<code>.level` (one of five
/// strings) and only right for the boolean keys by coincidence; and
/// `mode` was special-cased to insert its single legal keyword. Both now
/// fall out of the type: `mode`'s `Keyword` has exactly one spelling, so
/// it still inserts `mode = replace`.
///
/// A placeholder that must be quoted to parse is quoted — see
/// [`NameKind::quoted_in_ids`]. Inserting `diagnostics.<code>.level`
/// unquoted produced a syntax error.
fn settings_insert_text(segments: &[&str], value: &ValueType) -> String {
    let mut n = 0;
    let mut out = String::new();
    for &segment in segments {
        if !out.is_empty() {
            out.push('.');
        }
        match NameKind::from_placeholder(segment) {
            Some(kind) => {
                n += 1;
                let stop = format!("${{{n}:{}}}", kind.hint());
                if kind.quoted_in_ids() {
                    out.push_str(&format!("\"{stop}\""));
                } else {
                    out.push_str(&stop);
                }
            }
            None => out.push_str(segment),
        }
    }
    match value_snippet(value, n + 1) {
        Some(v) => out.push_str(&format!(" = {v}$0")),
        None => out.push_str("$0"),
    }
    out
}

/// The dotted settings-key path already typed immediately before the
/// cursor — e.g. `["validators", ""]` right after `validators.`, or
/// `["v"]` for a bare partial word with no dot yet. Scanned directly off
/// `source` (not the comment/string-stripped text `determine_context`
/// uses): by the time this runs, the caller has already confirmed the
/// cursor isn't inside a comment or string. Always at least one (possibly
/// empty) entry, so the caller never needs to special-case "nothing typed
/// yet".
fn settings_path_prefix(source: &str, offset: usize) -> Vec<String> {
    let end = offset.min(source.len());
    // A diagnostic code id has to be written as a quoted segment, so the
    // scan has to accept `"`, `:` and `-` too. Stopping at them meant that
    // typing a real code - the only way this key is ever written - left
    // the scan seeing just `.`, and every entry was then filtered out as
    // being on the wrong branch.
    let start = source[..end]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_alphanumeric() || matches!(c, '_' | '.' | '"' | ':' | '-'))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(end);

    // Split on `.` outside quotes, so a quoted segment containing one
    // stays whole, then unquote - the caller compares against unquoted
    // registry segments.
    let mut segments = vec![String::new()];
    let mut in_quotes = false;
    for c in source[start..end].chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => segments.push(String::new()),
            _ => segments.last_mut().expect("seeded with one entry").push(c),
        }
    }
    segments
}

/// Whether `key_pattern` is still reachable given the key path already
/// typed before the cursor (`typed`, from [`settings_path_prefix`]), and
/// if so, the segments still left to offer. A `<name>` segment in the
/// pattern matches any typed segment (names are user-chosen); every other
/// segment must match the already-finished part of `typed` exactly, and
/// prefix-match its in-progress last part. `typed == [""]` (nothing typed
/// yet) always matches, returning the whole pattern unchanged.
///
/// This is what keeps completion scoped to the actual current context: once
/// `validators.` is typed, `mode` and `annotations.<name>.allowed` no
/// longer match (their first segment isn't `validators`), and what's
/// offered for `validators.<name>.allowed` is just `<name>.allowed` — not
/// the whole path again.
fn matching_remaining<'a>(key_pattern: &'a str, typed: &[String]) -> Option<Vec<&'a str>> {
    let pattern: Vec<&str> = key_pattern.split('.').collect();
    let done = &typed[..typed.len() - 1];
    let partial = typed.last().map(String::as_str).unwrap_or("");

    if pattern.len() <= done.len() {
        return None;
    }
    for (i, seg) in done.iter().enumerate() {
        if !keys::is_placeholder(pattern[i]) && pattern[i] != seg {
            return None;
        }
    }
    let next = pattern[done.len()];
    if !keys::is_placeholder(next) && !next.starts_with(partial) {
        return None;
    }
    Some(pattern[done.len()..].to_vec())
}

/// Settings-entry completions inside a `settings { ... }` block (named or
/// unnamed), sourced from [`comline_core::settings::catalog::block_summary`]
/// — the same catalog [`hover`] reads for a settings entry's own tooltip,
/// so the two can't describe one key two different ways. Scoped to the key
/// path already typed before `offset` (see [`matching_remaining`]): a
/// catalog entry that can't follow what's already there (wrong branch, or
/// its next segment doesn't match what's partially typed) isn't offered at
/// all, and the ones that are only show what's left to type, not the whole
/// dotted path again.
fn get_settings_completions(source: &str, offset: usize) -> Vec<CompletionItem> {
    let typed = settings_path_prefix(source, offset);
    let listing = comline_core::settings::catalog::block_summary();

    // A key may be written with a declaration path in front of it
    // (`struct.field.validators.allowed`), which `enforcement` honours and
    // the plain listing never mentioned. Offering every prefix crossed
    // with every key at the top level would bury the common case, so the
    // scoped forms are offered only once a decl path has actually been
    // typed - which is also the point where the unscoped patterns stop
    // matching and the list used to go empty.
    let (decl, _) =
        keys::DeclPath::split_prefix(&typed.iter().map(String::as_str).collect::<Vec<_>>());
    let patterns: Vec<(String, &comline_core::settings::catalog::FieldDoc)> = listing
        .iter()
        .flat_map(|f| {
            let mut forms = vec![(f.key_pattern.clone(), f)];
            if f.decl_scopable {
                for path in keys::DeclPath::ALL {
                    forms.push((format!("{}.{}", path.dotted(), f.key_pattern), f));
                }
            }
            forms
        })
        // With no decl path typed, offer only the unscoped forms; once one
        // is, offer the scoped ones that extend it.
        .filter(|(pattern, _)| {
            let starts_scoped = keys::DeclPath::ALL
                .iter()
                .any(|p| pattern.starts_with(&format!("{}.", p.dotted())));
            starts_scoped == decl.is_some()
        })
        .collect();

    patterns
        .iter()
        .filter_map(|(pattern, f)| {
            let remaining = matching_remaining(pattern, &typed)?;
            Some(CompletionItem {
                label: remaining.join("."),
                kind: Some(CompletionItemKind::PROPERTY),
                detail: Some(f.summary.to_string()),
                documentation: Some(lsp_types::Documentation::String(f.description.to_string())),
                insert_text: Some(settings_insert_text(&remaining, &f.value)),
                insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
                ..Default::default()
            })
        })
        .collect()
}

/// Annotation-key completions for `scope`, sourced from
/// [`annotations::KNOWN_ANNOTATIONS`] — the same table [`hover`] reads for
/// an annotation key's own tooltip, so the two can't describe one key two
/// different ways.
/// `source` with the line containing `offset` replaced by blanks - same
/// byte length, so every other span stays put.
fn blank_line_at(source: &str, offset: usize) -> String {
    let offset = offset.min(source.len());
    let start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = source[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(source.len());
    let mut out = String::with_capacity(source.len());
    out.push_str(&source[..start]);
    out.extend(
        source[start..end]
            .chars()
            .map(|c| if c == '\n' { c } else { ' ' }),
    );
    out.push_str(&source[end..]);
    out
}

/// The settings in force for this schema: the package manifest's (or
/// Comline's built-in defaults when no manifest is in view) with the
/// file's own `settings { ... }` block layered on top. The same resolution
/// `hover` does, so the two agree on whether an annotation is permitted.
///
/// Declaration-scoped keys are deliberately not resolved here - see
/// [`forbidding_key`].
fn effective_settings<S: ProjectSource>(
    source: &str,
    offset: usize,
    other_files: &[S],
) -> comline_core::settings::SettingsDict {
    use comline_core::schema::ir::compiler::interpreter::incremental::IncrementalInterpreter;
    use comline_core::schema::ir::compiler::Compile;
    use comline_core::settings::effective::{effective_schema_settings, schema_settings};

    let package = other_files
        .iter()
        .find(|f| f.dependency().is_none() && crate::analysis::source::is_manifest(f.uri()))
        .map(|f| crate::analysis::source::declared_settings(f.text()))
        .unwrap_or_else(crate::analysis::source::base_settings);

    // The half-typed annotation the cursor is on does not parse, and a
    // document with one parse error is no document at all - so asking
    // about the file as it stands yields nothing, which is exactly when
    // this is called. Blank that one line and ask about the rest: the
    // `settings` block the answer depends on is elsewhere in the file.
    let document = parser::parse(source)
        .ok()
        .and_then(|r| r.document)
        .or_else(|| {
            let without_cursor_line = blank_line_at(source, offset);
            parser::parse(&without_cursor_line)
                .ok()
                .and_then(|r| r.document)
        });
    let Some(document) = document else {
        return package;
    };
    let units = IncrementalInterpreter::from_declarations(document.0);
    let schema = schema_settings(&units);
    effective_schema_settings(&package, &schema)
}

/// The snippet an annotation's dictionary form inserts, when it has one with
/// keys beyond the required: every key, each its own tab-stop, values as
/// choice lists (`@handshake = { name = "comline@2", scope = protocol, ... }`).
/// The plain completion inserts the plainest form; this one is offered
/// beside it, so the parameters can be found without the docs.
fn dict_form_insert_text(info: &annotations::AnnotationInfo) -> Option<String> {
    let ValueType::AnyOf(types) = &info.value else {
        return None;
    };
    let keys = types.iter().find_map(|t| match t {
        ValueType::Dict(keys) if keys.iter().any(|k| !k.required) => Some(*keys),
        _ => None,
    })?;
    let entries: Vec<String> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| match value_snippet(&k.value, i + 1) {
            Some(value) => format!("{} = {value}", k.key),
            None => k.key.to_string(),
        })
        .collect();
    Some(format!("{} = {{ {} }}$0", info.key, entries.join(", ")))
}

fn get_annotation_completions(
    scope: AnnotationScope,
    effective: &comline_core::settings::SettingsDict,
) -> Vec<CompletionItem> {
    annotations::for_scope(scope)
        .flat_map(|info| {
            let plain = annotation_completion(info, scope, effective, annotation_insert_text(info));
            let dict = dict_form_insert_text(info).map(|snippet| {
                let mut item = annotation_completion(info, scope, effective, Some(snippet));
                item.label = format!("{} {{ … }}", info.key);
                item.filter_text = Some(info.key.to_string());
                item.sort_text = Some(format!("{}~", info.key));
                item.detail = item
                    .detail
                    .map(|d| format!("dictionary form, with every key: {d}"));
                item
            });
            std::iter::once(plain).chain(dict)
        })
        .collect()
}

/// One completion item for `@key`, inserting `snippet`, marked when the
/// effective settings forbid it here.
fn annotation_completion(
    info: &annotations::AnnotationInfo,
    scope: AnnotationScope,
    effective: &comline_core::settings::SettingsDict,
    snippet: Option<String>,
) -> CompletionItem {
    let forbidden = forbidding_key(info.key, scope, effective);
    CompletionItem {
        label: info.key.to_string(),
        kind: Some(CompletionItemKind::PROPERTY),
        detail: Some(match &forbidden {
            Some(key) => format!("forbidden here by `{key}` — {}", info.description),
            None => info.description.to_string(),
        }),
        // Shown but marked, not hidden: the settings forbidding it
        // may well be the thing the author wants to change, and a
        // key that silently vanishes from the list is not a usable
        // explanation of why.
        tags: forbidden
            .as_ref()
            .map(|_| vec![lsp_types::CompletionItemTag::DEPRECATED]),
        insert_text_format: snippet
            .as_ref()
            .map(|_| lsp_types::InsertTextFormat::SNIPPET),
        insert_text: snippet,
        ..Default::default()
    }
}

/// The settings key that forbids `@key` at `scope`, if one does.
///
/// Resolved through the registry's own candidate order, the same one
/// `enforcement` consults, so the label cannot contradict the build. A
/// declaration-scoped key can allow an annotation the schema-level key
/// forbids, and completion only knows the *scope* a cursor is in, not
/// which declaration - so an annotation is called forbidden only when
/// every declaration path with that scope forbids it. Anything less
/// conservative would mark keys the build then accepts.
fn forbidding_key(
    key: &str,
    scope: AnnotationScope,
    effective: &comline_core::settings::SettingsDict,
) -> Option<String> {
    use comline_core::settings::value::SettingsValue;

    let shape = keys::annotations_allowed();
    let paths: Vec<keys::DeclPath> = keys::DeclPath::ALL
        .into_iter()
        .filter(|p| p.annotation_scope() == scope)
        .collect();

    // `@settings` is `Universal` - no declaration path has that scope, so
    // only the unscoped form applies.
    let candidate_sets: Vec<Vec<String>> = if paths.is_empty() {
        vec![vec![shape.key(None, Some(key))]]
    } else {
        paths.iter().map(|p| shape.candidates(*p, key)).collect()
    };

    let mut forbidding = None;
    for candidates in candidate_sets {
        // First key with an opinion wins, exactly as enforcement resolves.
        let verdict = candidates
            .iter()
            .find_map(|candidate| match effective.get_path(candidate) {
                Some(SettingsValue::Bool(allowed)) => Some((*allowed, candidate.clone())),
                _ => None,
            });
        match verdict {
            Some((false, candidate)) => forbidding = Some(candidate),
            // Allowed, or no key at all: allowed by default.
            _ => return None,
        }
    }
    forbidding
}

/// Completion context
#[derive(Debug, PartialEq)]
enum CompletionContext {
    /// After `:` / `->`, or inside an unclosed `union(...)` / function
    /// argument list.
    TypePosition,
    /// No enclosing block, and not naming something — a new top-level
    /// declaration can start here.
    TopLevel,
    /// Inside a `struct`/`error` body, before a field's name.
    StructBody,
    /// Inside a `protocol` body, before a member.
    ProtocolBody,
    /// Right after a bare `@` — naming an annotation key, for the given
    /// scope.
    AnnotationKey(AnnotationScope),
    /// Typing an arbitrary new name: right after a declaration keyword, or
    /// an enum variant. Nothing to suggest.
    DeclarationName,
    /// After a validator's name, where only `for` can come.
    ValidatorFor,
    /// Inside a validator's body.
    ValidatorBody(validator_body::Body),
    /// After one of a validator's target types: a `,` or the body's `{`
    /// comes next. Nothing to suggest.
    AfterValidatorTarget,
    /// Inside a `settings { ... }` block (named or unnamed), before an
    /// entry's dotted key.
    SettingsBody,
    /// An entry of a `@validators = [...]` list: a validator's name.
    ValidatorName,
    /// Inside the parentheses of a `@validators` entry, before an argument's
    /// name: `validator` as written, and the arguments already given.
    ValidatorArgument {
        validator: String,
        written: Vec<String>,
    },
    /// Context couldn't be narrowed down.
    Unknown,
}

/// The keyword of the declaration an annotation at `offset` attaches to:
/// the first word after the rest of this line and any further `@...`
/// lines. `None` at the end of the file. `cleaned` has comments (and so
/// docstrings) already blanked out.
fn declaration_after(cleaned: &str, offset: usize) -> Option<&str> {
    let rest = &cleaned[offset..];
    let after_line = rest.find('\n').map_or("", |i| &rest[i + 1..]);
    let line = after_line
        .lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with('@'))?;
    line.split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
}
