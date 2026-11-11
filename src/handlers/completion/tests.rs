use super::*;

#[test]
fn test_keyword_completions() {
    let completions = get_keyword_completions();
    assert!(completions.len() >= 5);
    assert!(completions.iter().any(|c| c.label == "struct"));
    assert!(completions.iter().any(|c| c.label == "enum"));
    assert!(completions.iter().any(|c| c.label == "protocol"));
    assert!(completions.iter().any(|c| c.label == "type"));
    assert!(completions.iter().any(|c| c.label == "error"));
    assert!(completions.iter().any(|c| c.label == "settings"));
    assert!(completions.iter().any(|c| c.label == "validator"));
}
/// Completion labels at the end of `source`.
fn labels_at_end(source: &str) -> Vec<String> {
    let uri = Url::parse("file:///test.ids").unwrap();
    let line = source.lines().count().saturating_sub(1) as u32;
    let column = source.rsplit('\n').next().unwrap_or("").len() as u32;
    let line = if source.ends_with('\n') {
        line + 1
    } else {
        line
    };
    get_completions(source, &uri, Position::new(line, column))
        .into_iter()
        .map(|c| c.label)
        .collect()
}
#[test]
fn after_a_validators_name_only_for_is_offered() {
    // Offered every top-level keyword, none of which can go there.
    assert_eq!(labels_at_end("validator Positive "), ["for"]);
    assert_eq!(
        labels_at_end("type N = u8\n\nvalidator Positive f"),
        ["for"]
    );
}
#[test]
fn accepting_for_reopens_the_suggestions_on_the_types() {
    let uri = Url::parse("file:///test.ids").unwrap();
    let item = get_completions("validator Positive ", &uri, Position::new(0, 19)).remove(0);
    assert_eq!(item.insert_text.as_deref(), Some("for "));
    assert_eq!(
        item.command.map(|c| c.command).as_deref(),
        Some("editor.action.triggerSuggest")
    );
}
#[test]
fn after_for_and_each_comma_the_types_are_offered() {
    for source in [
        "type FixedNum = u8\n\nvalidator Positive for ",
        "type FixedNum = u8\n\nvalidator Positive for u8, ",
        "type FixedNum = u8\n\nvalidator Positive for u8, s",
    ] {
        let labels = labels_at_end(source);
        assert!(labels.iter().any(|l| l == "u8"), "{source:?}: {labels:?}");
        assert!(
            labels.iter().any(|l| l == "string"),
            "{source:?}: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "FixedNum"),
            "{source:?}: {labels:?}"
        );
        for keyword in ["const", "struct", "validator", "for"] {
            assert!(
                !labels.iter().any(|l| l == keyword),
                "{source:?} offered {keyword}: {labels:?}"
            );
        }
    }
}
#[test]
fn a_document_mid_edit_still_offers_its_own_types() {
    // The trailing field doesn't parse, so there's no symbol table; the
    // declarations are found by scanning instead. A name inside a block
    // isn't a declaration.
    let labels = labels_at_end(
        "type Id = u64\nenum Mode { Fast }\nstruct S {\n    struct_like: u8\n    a: ",
    );
    assert!(labels.iter().any(|l| l == "Id"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Mode"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "S"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "Fast"), "{labels:?}");
}
#[test]
fn after_a_target_type_nothing_is_offered() {
    // A `,` or the body's `{` comes next.
    assert!(labels_at_end("validator Positive for u8 ").is_empty());
}
#[test]
fn a_validators_body_and_what_follows_it_are_unaffected() {
    // A property's type, after `:`.
    let labels = labels_at_end("validator Range for u8 {\n    max: ");
    assert!(labels.iter().any(|l| l == "u8"), "{labels:?}");
    // A new declaration after a finished validator.
    let labels = labels_at_end("validator Range for u8 {\n}\n\n");
    assert!(labels.iter().any(|l| l == "struct"), "{labels:?}");
}
const BODY_HEAD: &str = "type Name = string\n\nvalidator Bounds for u32, Name {\n    min: u32 = 0\n    label: string\n";
#[test]
fn a_validator_body_offers_validate_where_a_member_starts() {
    // Offered every top-level keyword (`validator` for a `v`).
    assert_eq!(labels_at_end(&format!("{BODY_HEAD}    v")), ["validate"]);
    assert_eq!(labels_at_end("validator V for u8 {\n    "), ["validate"]);
    // A second `validate` block isn't valid.
    let source = format!("{BODY_HEAD}    validate {{\n    }}\n    ");
    assert!(
        labels_at_end(&source).is_empty(),
        "{:?}",
        labels_at_end(&source)
    );
}
#[test]
fn a_property_takes_a_type_and_nothing_else_is_offered() {
    let labels = labels_at_end(&format!("{BODY_HEAD}    max: "));
    assert!(
        labels.iter().any(|l| l == "u32") && labels.iter().any(|l| l == "Name"),
        "{labels:?}"
    );
    // After a property's type, and in its default: nothing.
    assert!(labels_at_end(&format!("{BODY_HEAD}    max: u32 ")).is_empty());
    assert!(labels_at_end(&format!("{BODY_HEAD}    max: u32 = ")).is_empty());
}
#[test]
fn a_validate_block_offers_assert() {
    assert_eq!(
        labels_at_end(&format!("{BODY_HEAD}    validate {{\n        ")),
        ["assert"]
    );
    assert_eq!(
        labels_at_end(&format!("{BODY_HEAD}    validate {{\n        a")),
        ["assert"]
    );
    let after_one =
        format!("{BODY_HEAD}    validate {{\n        assert(value > 0, \"x\")\n        ");
    assert_eq!(labels_at_end(&after_one), ["assert"]);
}
#[test]
fn a_condition_offers_its_roots_accessors_and_properties() {
    let validate = format!("{BODY_HEAD}    validate {{\n        assert(");
    assert_eq!(labels_at_end(&validate), ["value", "params"]);
    assert_eq!(
        labels_at_end(&format!("{validate}value >= ")),
        ["value", "params"]
    );
    // `Name` is an alias of `string`, so `length` may apply.
    assert_eq!(
        labels_at_end(&format!("{validate}value.")),
        ["length", "name"]
    );
    assert_eq!(
        labels_at_end(&format!("{validate}params.")),
        ["min", "label"]
    );
    assert_eq!(
        labels_at_end(&format!("{validate}value > params.min ")),
        ["and", "or"]
    );
    assert_eq!(
        labels_at_end(&format!("{validate}value > 1 and ")),
        ["value", "params"]
    );
    // The message is a string.
    assert!(labels_at_end(&format!("{validate}value > 1, ")).is_empty());
}
#[test]
fn length_is_offered_only_when_a_target_can_have_one() {
    let numbers = "validator Positive for u8, s32 {\n    validate {\n        assert(value.";
    assert_eq!(labels_at_end(numbers), ["name"]);
    let text = "validator Short for string {\n    validate {\n        assert(value.";
    assert_eq!(labels_at_end(text), ["length", "name"]);
}
#[test]
fn the_params_item_reopens_on_the_properties() {
    let uri = Url::parse("file:///test.ids").unwrap();
    let source = "validator V for u8 {\n    validate {\n        assert(";
    let items = get_completions(source, &uri, Position::new(2, 15));
    let params = items.into_iter().find(|c| c.label == "params").unwrap();
    assert_eq!(params.insert_text.as_deref(), Some("params."));
    assert!(params.command.is_some());
}
#[test]
fn the_validator_snippet_writes_for() {
    // Without it, the inserted validator fails the build: it must name
    // the type it checks.
    let validator = get_keyword_completions()
        .into_iter()
        .find(|c| c.label == "validator")
        .unwrap();
    assert_eq!(
        validator.insert_text.as_deref(),
        Some("validator $1 for $2 {\n\t$0\n}")
    );
}
#[test]
fn test_primitive_type_completions() {
    let completions = get_primitive_type_completions();
    assert!(completions.len() >= 10);
    assert!(completions.iter().any(|c| c.label == "s32"));
    assert!(completions.iter().any(|c| c.label == "string"));
    assert!(completions.iter().any(|c| c.label == "bool"));
    // The actual regression check: `i8`/`i16`/`i32`/`i64` are not real
    // Comline syntax and must never be offered.
    for fake in ["i8", "i16", "i32", "i64"] {
        assert!(
            !completions.iter().any(|c| c.label == fake),
            "offered {fake}"
        );
    }
}
#[test]
fn keyword_completions_include_use_and_import() {
    // Both are `KeywordKind::Declaration` in `vocabulary` but were
    // missing from the old hand-maintained list's equivalent check in
    // some call paths — explicit regression guard.
    let completions = get_keyword_completions();
    assert!(completions.iter().any(|c| c.label == "use"));
    assert!(completions.iter().any(|c| c.label == "import"));
    // `function` is a real keyword but not a top-level declaration —
    // it only belongs inside a protocol body.
    assert!(!completions.iter().any(|c| c.label == "function"));
}
#[test]
fn typing_a_use_path_offers_path_starts_not_everything() {
    // Once fell through every specific check to `Unknown`, dumping
    // keywords + primitives + every known type after `use `.
    let source = "use pk";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(0, 6);
    let labels: Vec<String> = get_completions(source, &uri, position)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(labels, ["self", "parent", "package", "std"]);
}
#[test]
fn test_completion_after_colon() {
    let source = "struct User {\n    name: ";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 10); // After ":"
    let completions = get_completions(source, &uri, position);
    // Should include types
    assert!(!completions.is_empty());
    assert!(completions.iter().any(|c| c.label == "string"));
}
#[test]
fn test_completion_top_level() {
    let source = "\n";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(0, 0);
    let completions = get_completions(source, &uri, position);
    // Should include keywords
    assert!(completions.iter().any(|c| c.label == "struct"));
    assert!(completions.iter().any(|c| c.label == "enum"));
}
#[test]
fn top_level_after_closed_declarations_still_offers_keywords() {
    // A full document that ends with a trailing partial identifier at
    // top level (the common "actively typing a new declaration"
    // shape) fails to parse as a whole — context detection must not
    // depend on the document parsing successfully.
    let source = "enum Language {\n    English\n}\n\nstruct Greeting {\n    message: string\n}\n\nprotocol Thing {\n    function foo();\n}\n\nty";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(11, 2);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "type"));
    assert!(completions.iter().any(|c| c.label == "struct"));
    // No primitive types at top level - "u32" etc. can never start a
    // top-level declaration.
    assert!(!completions.iter().any(|c| c.label == "u32"));
}
#[test]
fn protocol_body_offers_function_not_types_or_top_level_keywords() {
    let source = "protocol Thing {\n    function foo();\n    f";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(2, 5);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "function"));
    assert!(!completions.iter().any(|c| c.label == "struct"));
    assert!(!completions.iter().any(|c| c.label == "u32"));
}
#[test]
fn typing_a_function_name_offers_nothing() {
    let source = "protocol Thing {\n    function f";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 13);
    assert!(get_completions(source, &uri, position).is_empty());
}
#[test]
fn typing_any_declaration_name_offers_nothing() {
    let uri = Url::parse("file:///test.ids").unwrap();
    for source in ["struct Us", "enum Sta", "const MA", "type Use", "error No"] {
        let position = Position::new(0, source.chars().count() as u32);
        assert!(
            get_completions(source, &uri, position).is_empty(),
            "expected no completions while typing the name in {source:?}"
        );
    }
}
#[test]
fn enum_body_offers_nothing() {
    let source = "enum Status {\n    Active\n    P";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(2, 5);
    assert!(get_completions(source, &uri, position).is_empty());
}
#[test]
fn named_settings_body_offers_catalog_keys_not_global_keywords() {
    // Direct regression guard: before `EnclosingBlock::Settings`
    // existed, a named block fell through to `CompletionContext::Unknown`
    // (the "walk back past the block's own name" logic skipped past
    // "Test" expecting one more Word behind it, found none matching
    // any known keyword) and offered the same broad keyword/type mix
    // as anywhere else - "validator" (a declaration keyword) showed up
    // for a bare "v".
    let source = "settings Test {\n    v";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    let completions = get_completions(source, &uri, position);
    assert!(
        completions
            .iter()
            .any(|c| c.label == "validators.<name>.allowed"),
        "{completions:?}"
    );
    assert!(
        completions.iter().any(|c| c.label == "validators.allowed"),
        "{completions:?}"
    );
    assert!(
        !completions.iter().any(|c| c.label == "validator"),
        "{completions:?}"
    );
    assert!(
        !completions.iter().any(|c| c.label == "struct"),
        "{completions:?}"
    );
}
#[test]
fn unnamed_settings_body_offers_catalog_keys() {
    // The unnamed case needed its own fix: the "skip the block's own
    // name" step would otherwise skip past `settings` itself (the
    // only Word between the keyword and `{` when there's no name),
    // landing one token too far back.
    let source = "settings {\n    an";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 6);
    let completions = get_completions(source, &uri, position);
    assert!(
        completions
            .iter()
            .any(|c| c.label == "annotations.<name>.allowed"),
        "{completions:?}"
    );
}
#[test]
fn after_a_dot_only_the_remaining_segment_is_offered_not_the_whole_path_again() {
    // The reported bug: typing `validators.` re-offered the whole
    // `validators.<name>.allowed` / `validators.allowed` labels, plus
    // entirely unrelated entries (`mode`, `annotations.<name>.allowed`)
    // — because completion never looked at what was already typed.
    // Accepting one of the old labels here would have printed
    // `validators.` twice.
    let source = "settings Test {\n    validators.";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 15);
    let completions = get_completions(source, &uri, position);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"<name>.allowed"), "{labels:?}");
    assert!(labels.contains(&"allowed"), "{labels:?}");
    assert!(!labels.contains(&"validators.<name>.allowed"), "{labels:?}");
    assert!(!labels.contains(&"validators.allowed"), "{labels:?}");
    assert!(!labels.contains(&"mode"), "{labels:?}");
    assert!(
        !labels.iter().any(|l| l.starts_with("annotations")),
        "{labels:?}"
    );
}
#[test]
fn a_partial_segment_after_a_dot_filters_by_its_own_prefix() {
    // `al` could still become either the literal `allowed` (the coarse
    // form) or the start of a validator's own name (`<name>.allowed`)
    // — both stay candidates; unrelated catalog branches don't.
    let source = "settings Test {\n    validators.al";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 17);
    let completions = get_completions(source, &uri, position);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"allowed"), "{labels:?}");
    assert!(labels.contains(&"<name>.allowed"), "{labels:?}");
    assert_eq!(labels.len(), 2, "{labels:?}");
}
#[test]
fn settings_insert_text_after_a_dot_only_fills_in_the_rest() {
    let source = "settings Test {\n    validators.";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 15);
    let completions = get_completions(source, &uri, position);
    let allowed = completions
        .iter()
        .find(|c| c.label == "allowed")
        .expect("allowed should be offered");
    assert_eq!(
        allowed.insert_text.as_deref(),
        Some("allowed = ${1:true}$0")
    );
}
#[test]
fn settings_completions_are_sourced_from_the_catalog_as_snippets() {
    let source = "settings {\n    m";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    let completions = get_completions(source, &uri, position);
    let mode = completions
        .iter()
        .find(|c| c.label == "mode")
        .expect("mode should be offered");
    // `mode`'s single legal keyword is still inserted finished rather
    // than as a one-item choice list - it just falls out of
    // `Keyword(["replace"])` now instead of being special-cased. The
    // trailing `$0` is the final cursor stop every entry now ends with.
    assert_eq!(mode.insert_text.as_deref(), Some("mode = replace$0"));
    assert_eq!(
        mode.insert_text_format,
        Some(lsp_types::InsertTextFormat::SNIPPET)
    );
    assert_eq!(mode.kind, Some(CompletionItemKind::PROPERTY));
    assert!(mode.detail.is_some());
}
#[test]
fn a_value_of_several_forms_inserts_the_plainest_and_a_dictionary_its_required_keys() {
    use comline_core::config_value::DictKey;
    // `@framing` takes a name or a dictionary; the name is what's typed.
    let framing = annotations::lookup("framing").unwrap();
    assert_eq!(
        annotation_insert_text(framing).as_deref(),
        Some("framing = \"${1|jsonrpc,datagram,datagram@2,datagram@3|}\"$0")
    );
    const KEYS: &[DictKey] = &[
        DictKey {
            key: "name",
            value: ValueType::StrEnum(&[]),
            required: true,
        },
        DictKey {
            key: "level",
            value: ValueType::Integer,
            required: true,
        },
        DictKey {
            key: "note",
            value: ValueType::Str,
            required: false,
        },
    ];
    assert_eq!(
        value_snippet(&ValueType::Dict(KEYS), 1).as_deref(),
        Some("{ name = \"${1||}\", level = ${2:0} }")
    );
}
#[test]
fn framing_is_marked_forbidden_in_a_default_package() {
    // The built-in default forbids `@framing`, so the editor used to
    // suggest an annotation that produces a hard build error the
    // moment it is accepted.
    let source = "@\nprotocol Thing {\n}\n";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(0, 1);
    let completions = get_completions(source, &uri, position);
    let framing = completions
        .iter()
        .find(|c| c.label == "framing")
        .expect("framing should still be offered, just marked");
    assert!(
        framing
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("forbidden here by"),
        "got: {:?}",
        framing.detail
    );
    assert_eq!(
        framing.tags,
        Some(vec![lsp_types::CompletionItemTag::DEPRECATED])
    );
}
#[test]
fn framing_is_unmarked_once_the_schema_allows_it() {
    let source = "@\nprotocol Thing {\n}\n\n\
                  settings {\n    annotations.framing.allowed = true\n}\n";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(0, 1);
    let completions = get_completions(source, &uri, position);
    let framing = completions
        .iter()
        .find(|c| c.label == "framing")
        .expect("offered");
    assert_eq!(framing.tags, None, "got: {:?}", framing.detail);
}
#[test]
fn an_ordinary_annotation_is_not_marked() {
    let source = "protocol Thing {\n    @\n    function f();\n}\n";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    let completions = get_completions(source, &uri, position);
    let idempotent = completions
        .iter()
        .find(|c| c.label == "idempotent")
        .expect("idempotent should be offered");
    assert_eq!(idempotent.tags, None);
}
#[test]
fn a_decl_scoped_key_is_offered_once_a_decl_path_is_typed() {
    // `enforcement` honours `struct.field.validators.allowed`, but the
    // editor offered nothing here and the list went empty - the
    // unscoped patterns all start with a group name, so typing
    // `struct.` matched none of them.
    let source = "settings {\n    struct.field.";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"validators.allowed"), "{labels:?}");
    assert!(labels.contains(&"annotations.<name>.allowed"), "{labels:?}");
    // `diagnostics` is project-wide only, so it must not be offered
    // under a declaration path.
    assert!(
        !labels.iter().any(|l| l.starts_with("diagnostics")),
        "{labels:?}"
    );
}
#[test]
fn the_top_level_listing_is_not_buried_by_the_scoped_forms() {
    // Six decl paths crossed with every scopable key would swamp the
    // common case, so the scoped forms appear only once a path is
    // typed.
    let source = "settings {\n    ";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 4);
    let completions = get_completions(source, &uri, position);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        !labels.iter().any(|l| l.starts_with("struct.")),
        "scoped forms should not be offered unprompted: {labels:?}"
    );
    assert!(labels.contains(&"validators.allowed"), "{labels:?}");
}
#[test]
fn the_diagnostics_level_key_inserts_a_usable_entry() {
    // Triple-broken before: it inserted the literal text `<code>` with
    // no tab-stop, unquoted (so it did not parse), and `= true` where
    // one of five level strings is required.
    let source = "settings {\n    d";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    let completions = get_completions(source, &uri, position);
    let level = completions
        .iter()
        .find(|c| c.label == "diagnostics.<code>.level")
        .expect("the diagnostics level key should be offered");
    assert_eq!(
        level.insert_text.as_deref(),
        Some(
            "diagnostics.\"${1:namespace::code}\".level = \
             \"${2|allow,hint,info,warn,error|}\"$0"
        )
    );
}
#[test]
fn the_diagnostics_level_key_survives_typing_past_its_group() {
    // `matching_remaining` treated only the literal `<name>` as a
    // wildcard, so this entry was filtered out the moment anything was
    // typed after `diagnostics.` - exactly when it is most wanted.
    let source = "settings {\n    diagnostics.\"validation::deprecated-type-usage\".";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"level"), "{labels:?}");
}
#[test]
fn a_bool_key_and_a_level_key_do_not_insert_the_same_value() {
    // The editor assumed every settings key took a boolean. The value
    // half now comes from each key's own `ValueType`.
    let bool_key = settings_insert_text(&["allowed"], &ValueType::Bool);
    assert_eq!(bool_key, "allowed = ${1:true}$0");
    let level = comline_core::settings::catalog::block_summary()
        .iter()
        .find(|d| d.key_pattern == "diagnostics.<code>.level")
        .expect("the registry lists it");
    assert!(
        settings_insert_text(&["level"], &level.value).contains("allow,hint"),
        "a level key offers the level set, not a boolean"
    );
}
#[test]
fn handshakes_dictionary_form_is_offered_with_every_parameter() {
    let info = annotations::lookup("handshake").expect("handshake is an annotation");
    let plain = annotation_insert_text(info).unwrap();
    assert!(
        plain.starts_with("handshake = \"${1|") && plain.contains("comline@2"),
        "{plain}"
    );
    let dict = dict_form_insert_text(info).expect("handshake has a dictionary form");
    assert!(dict.starts_with("handshake = { name = \"${1|"), "{dict}");
    for part in [
        "scope = ${2|protocol,package|}",
        "newer_consumer = ${3|degrade,refuse|}",
        "same_version_differs = ${4|refuse,warn|}",
        "exchange = ${5|offer,symmetric|}",
    ] {
        assert!(dict.contains(part), "{part} missing from {dict}");
    }
    // Offered beside the plain form, under the same name.
    let items = get_annotation_completions(AnnotationScope::Leading, &Default::default());
    let forms: Vec<&CompletionItem> = items
        .iter()
        .filter(|c| c.filter_text.as_deref().unwrap_or(&c.label) == "handshake")
        .collect();
    assert_eq!(forms.len(), 2, "{forms:?}");
    // An annotation with no optional keys has only the plain form.
    let deprecated = annotations::lookup("deprecated").unwrap();
    assert_eq!(dict_form_insert_text(deprecated), None);
}
#[test]
fn framings_choice_list_comes_from_core_not_a_local_copy() {
    // This used to be a hardcoded `jsonrpc,datagram` here. Core is the
    // authority on the set, so the two cannot drift.
    let info = annotations::for_scope(AnnotationScope::Leading)
        .find(|i| i.key == "framing")
        .expect("framing is a protocol annotation");
    let snippet = annotation_insert_text(info).expect("framing takes a value");
    for spelling in config_value::spellings(&info.value) {
        assert!(
            snippet.contains(spelling),
            "{spelling} missing from {snippet}"
        );
    }
}
/// Completion labels at the end of `line` (0-based) of `source`.
fn labels_at_end_of(source: &str, line: u32) -> Vec<String> {
    let uri = Url::parse("file:///test.ids").unwrap();
    let text = source.lines().nth(line as usize).unwrap();
    let position = Position::new(line, text.chars().count() as u32);
    get_completions(source, &uri, position)
        .into_iter()
        .map(|c| c.label)
        .collect()
}
#[test]
fn above_a_type_alias_validators_and_deprecated_are_offered() {
    let source = "@\n@deprecated\ntype Name = string\n";
    let labels = labels_at_end_of(source, 0);
    assert!(
        labels.iter().any(|l| l.contains("validators")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("deprecated")),
        "{labels:?}"
    );
    assert!(!labels.iter().any(|l| l.contains("framing")), "{labels:?}");
}
#[test]
fn above_a_struct_validators_are_not_offered() {
    let labels = labels_at_end_of("@\nstruct S {\n}\n", 0);
    assert!(
        !labels.iter().any(|l| l.contains("validators")),
        "{labels:?}"
    );
    assert!(labels.iter().any(|l| l.contains("framing")), "{labels:?}");
}
#[test]
fn a_marker_annotation_inserts_no_value_half() {
    let info = annotations::for_scope(AnnotationScope::Function)
        .find(|i| i.key == "idempotent")
        .expect("idempotent is a function annotation");
    assert_eq!(
        annotation_insert_text(info),
        None,
        "a marker takes no `= value`"
    );
}
#[test]
fn type_position_survives_interior_whitespace() {
    let source = "struct X {\n    test:  "; // two trailing spaces
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "u32"));
}
#[test]
fn type_position_survives_a_partial_type_name() {
    let source = "struct X {\n    test: u";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "u64"));
}
#[test]
fn union_member_position_offers_types() {
    let source = "struct X {\n    v: union(";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "u32"));
}
#[test]
fn return_type_position_offers_types() {
    let source = "protocol P {\n    function f() -> ";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "string"));
}
#[test]
fn function_arg_list_offers_types_not_declaration_keywords() {
    // No `name:` yet - a bare type is also valid grammar here.
    let source = "protocol P {\n    function f(";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, source.lines().last().unwrap().chars().count() as u32);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "u32"));
    assert!(!completions.iter().any(|c| c.label == "struct"));
}
#[test]
fn annotation_on_a_field_offers_validators() {
    let source = "struct X {\n    @";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "validators"));
    // Function/leading-only keys shouldn't leak into field scope.
    assert!(!completions.iter().any(|c| c.label == "timeout_ms"));
    assert!(!completions.iter().any(|c| c.label == "framing"));
}
#[test]
fn annotation_on_a_function_offers_timeout_ms_and_idempotent() {
    let source = "protocol P {\n    @";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "timeout_ms"));
    assert!(!completions.iter().any(|c| c.label == "validators"));
    // `idempotent` is a bare marker — no `=value` snippet, just the
    // key name itself (a client inserts `label` verbatim when
    // `insert_text` is unset).
    let idempotent = completions
        .iter()
        .find(|c| c.label == "idempotent")
        .expect("idempotent should be offered");
    assert!(idempotent.insert_text.is_none());
}
#[test]
fn annotation_at_top_level_offers_framing() {
    let source = "@";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(0, 1);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "framing"));
    assert!(!completions.iter().any(|c| c.label == "validators"));
    assert!(!completions.iter().any(|c| c.label == "timeout_ms"));
}
#[test]
fn annotation_key_still_resolves_while_partially_typed() {
    // Trailing partial word after `@` is dropped the same way any
    // other in-progress word is, same as `type_position_survives_a_
    // partial_type_name` for `:`.
    let source = "protocol P {\n    @time";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 9);
    let completions = get_completions(source, &uri, position);
    assert!(completions.iter().any(|c| c.label == "timeout_ms"));
}
#[test]
fn annotation_in_an_enum_body_offers_nothing() {
    let source = "enum Status {\n    @";
    let uri = Url::parse("file:///test.ids").unwrap();
    let position = Position::new(1, 5);
    assert!(get_completions(source, &uri, position).is_empty());
}
#[test]
fn no_completions_inside_comments_or_strings() {
    let uri = Url::parse("file:///test.ids").unwrap();
    // inside a `///` docstring
    let src = "/// Message that can be se\nstruct M {}\n";
    assert!(get_completions(src, &uri, Position::new(0, 25)).is_empty());
    // inside a `//` comment after code
    let src = "struct M { a: u8 } // note he\n";
    assert!(get_completions(src, &uri, Position::new(0, 27)).is_empty());
    // inside a string literal
    let src = "error E {\n    message = \"oops re\n}\n";
    assert!(get_completions(src, &uri, Position::new(1, 20)).is_empty());
    // code before a trailing comment on the same line still completes
    let src = "struct M {\n    name:  // a field\n}\n";
    assert!(!get_completions(src, &uri, Position::new(1, 10)).is_empty());
}
#[test]
fn strip_comments_and_strings_preserves_length_and_newlines() {
    let source = "struct M {\n    a: string // note\n    b: string\n}\n";
    let cleaned = strip_comments_and_strings(source);
    assert_eq!(cleaned.chars().count(), source.chars().count());
    assert_eq!(
        cleaned.chars().filter(|&c| c == '\n').count(),
        source.chars().filter(|&c| c == '\n').count()
    );
    assert!(!cleaned.contains("note"));
    assert!(cleaned.contains("struct M {"));
}
#[test]
fn strip_comments_and_strings_blanks_a_block_comment_spanning_lines() {
    // A brace inside a commented-out declaration must not confuse the
    // backward brace-balance scan context detection relies on.
    let source = "/*\nerror Test {\n    message = \"\"\n}\n*/\nstruct M {\n    \n}\n";
    let cleaned = strip_comments_and_strings(source);
    assert_eq!(cleaned.chars().count(), source.chars().count());
    assert_eq!(
        cleaned.chars().filter(|&c| c == '\n').count(),
        source.chars().filter(|&c| c == '\n').count()
    );
    assert!(!cleaned.contains("error"));
    // Only `struct M {`'s brace should survive - the one inside the
    // block comment must be blanked out too.
    assert_eq!(cleaned.matches('{').count(), 1, "{cleaned:?}");
    assert!(cleaned.contains("struct M {"));
}
#[test]
fn completion_works_right_after_a_multi_line_block_comment() {
    // The regression this whole change is for: a block-commented-out
    // declaration sitting earlier in the file must not throw off
    // completion context for real code afterward.
    let source = "/*\nerror Test {\n    message = \"\"\n}\n*/\nstruct M {\n    \n}\n";
    let uri = Url::parse("file:///test.ids").unwrap();
    let items = get_completions(source, &uri, Position::new(6, 4));
    // StructBody context: the one modifier keyword a field can start
    // with. Confirms the commented-out `error Test { ... }` above
    // didn't throw off the brace-balance scan that finds this is
    // inside `struct M`'s body, not `error Test`'s.
    assert!(items.iter().any(|i| i.label == "optional"), "{items:?}");
}
#[test]
fn a_validators_list_is_its_own_context() {
    let at_end = |source: &str| determine_context(source, source.len());
    assert_eq!(at_end("@validators = ["), CompletionContext::ValidatorName);
    assert_eq!(
        at_end("@validators = [St"),
        CompletionContext::ValidatorName
    );
    assert_eq!(
        at_end("struct S {\n    @validators = [A(min = 1), "),
        CompletionContext::ValidatorName
    );
    assert_eq!(
        at_end("@validators = [A(min = 1, m"),
        CompletionContext::ValidatorArgument {
            validator: "A".into(),
            written: vec!["min".into()]
        }
    );
    // An argument's value: nothing to offer.
    assert_eq!(
        at_end("@validators = [A(min = "),
        CompletionContext::DeclarationName
    );
    // Closed: back to whatever follows.
    assert_eq!(at_end("@validators = [A]\n"), CompletionContext::TopLevel);
}
fn validator_completions(source: &str) -> Vec<CompletionItem> {
    let uri = Url::parse("file:///pkg/src/api.ids").unwrap();
    let std = crate::analysis::stdlib::files(&crate::analysis::stdlib::root());
    let position = crate::util::offset_to_position(source, source.find('|').unwrap());
    get_completions_with_project(&source.replace('|', ""), &uri, position, &std)
}
#[test]
fn a_validators_list_offers_validators_not_keywords() {
    let items = validator_completions(
        "validator Mine for string {\n}\n\n@validators = [St|]\ntype T = string\n",
    );
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"Mine"), "{labels:?}");
    assert!(labels.contains(&"StringBounds"), "{labels:?}");
    assert!(
        !labels.contains(&"struct") && !labels.contains(&"settings"),
        "{labels:?}"
    );
    // Not imported: it brings its `use` along, after what's in scope.
    let bounds = items.iter().find(|i| i.label == "StringBounds").unwrap();
    let edit = &bounds.additional_text_edits.as_ref().expect("adds the use")[0];
    assert_eq!(edit.new_text, "use std::validators::StringBounds\n\n");
    assert!(bounds
        .sort_text
        .as_deref()
        .unwrap_or_default()
        .starts_with('~'));
}
#[test]
fn an_imported_validator_needs_no_use() {
    let items = validator_completions(
        "use std::validators::StringBounds\n\n@validators = [|]\ntype T = string\n",
    );
    let bounds = items.iter().find(|i| i.label == "StringBounds").unwrap();
    assert!(bounds.additional_text_edits.is_none(), "{bounds:?}");
    assert_eq!(
        items.iter().filter(|i| i.label == "StringBounds").count(),
        1
    );
}
#[test]
fn a_required_property_goes_in_as_a_snippet() {
    let items = validator_completions("validator AtLeast for u32 {\n    min: u32\n    max: u32 = 9\n}\n\n@validators = [|\ntype T = u32\n");
    let at_least = items.iter().find(|i| i.label == "AtLeast").unwrap();
    assert_eq!(at_least.insert_text.as_deref(), Some("AtLeast(min = ${1})"));
    // All defaulted: bare.
    let bounds = items.iter().find(|i| i.label == "StringBounds").unwrap();
    assert_eq!(bounds.insert_text, None);
}
#[test]
fn inside_the_parentheses_the_properties_not_written_yet() {
    let items = validator_completions(
        "use std::validators::StringBounds\n\n@validators = [StringBounds(min_chars = 1, |)]\ntype T = string\n",
    );
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["max_chars"]);
    assert_eq!(items[0].insert_text.as_deref(), Some("max_chars = "));
    // Not imported: nothing in scope to complete.
    let items = validator_completions("@validators = [StringBounds(|)]\ntype T = string\n");
    assert!(items.is_empty(), "{items:?}");
}
#[test]
fn test_context_detection() {
    assert_eq!(
        determine_context("name: ", 6),
        CompletionContext::TypePosition
    );
    assert_eq!(determine_context("", 0), CompletionContext::TopLevel);
    assert_eq!(
        determine_context("struct User {\n    ", 18),
        CompletionContext::StructBody
    );
    assert_eq!(
        determine_context("protocol Thing {\n    ", 21),
        CompletionContext::ProtocolBody
    );
    assert_eq!(
        determine_context("struct ", 7),
        CompletionContext::DeclarationName
    );
}
const TYPES: &str = "struct Message {\n    text: string\n}\n\nenum Kind {\n    A\n}\n\nprotocol Api {\n    function f();\n}\n";
/// Completions at the end of `source` (a type position), with
/// `pkg/src/types.ids` open: `(label, detail, adds a use line?)` for every
/// item from another file.
fn imported_items(source: &str) -> Vec<(String, String, Option<String>)> {
    let uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
    let others = [(
        Url::parse("file:///pkg/src/types.ids").unwrap(),
        TYPES.to_string(),
    )];
    let end = crate::util::offset_to_position(source, source.len());
    get_completions_with_project(source, &uri, end, &others)
        .into_iter()
        .filter(|c| c.detail.as_deref().is_some_and(|d| d.contains("types.ids")))
        .map(|c| {
            let edit = c.additional_text_edits.map(|e| e[0].new_text.clone());
            (c.label, c.detail.unwrap(), edit)
        })
        .collect()
}
#[test]
fn imported_types_are_offered_and_the_rest_add_their_use() {
    // Mid-edit: the file doesn't parse, its `use` line still does.
    let items = imported_items("use types::Message\n\nstruct S {\n    m: ");
    assert_eq!(
        items,
        vec![
            ("Message".to_string(), "from `types.ids`".to_string(), None),
            (
                "Kind".to_string(),
                "from `types.ids` — adds `use types::Kind`".to_string(),
                Some("use types::Kind\n".to_string())
            ),
        ],
        "the protocol `Api` isn't a type"
    );
}
#[test]
fn an_alias_is_offered_under_its_alias_only() {
    let items = imported_items("use types::Message as Msg\n\nstruct S {\n    m: ");
    let labels: Vec<_> = items
        .iter()
        .map(|(l, d, _)| (l.as_str(), d.as_str()))
        .collect();
    assert!(
        labels.contains(&("Msg", "`Message` from `types.ids`")),
        "{items:?}"
    );
    assert!(
        !labels.iter().any(|(l, _)| *l == "Message"),
        "already imported as `Msg`: {items:?}"
    );
}
#[test]
fn a_glob_brings_in_every_type() {
    let items = imported_items("use types::*\n\nstruct S {\n    m: ");
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(items.iter().all(|(_, _, edit)| edit.is_none()), "{items:?}");
}
#[test]
fn without_a_use_every_type_adds_one_at_the_top() {
    let items = imported_items("struct S {\n    m: ");
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(items.iter().any(
        |(l, _, edit)| l == "Message" && edit.as_deref() == Some("use types::Message\n\n")
    ));
}
#[test]
fn a_type_declared_here_is_not_offered_from_another_file() {
    let source = "struct Message {\n    mine: bool\n}\n\nstruct S {\n    m: Message\n    n: ";
    let finished = format!("{source}bool\n}}\n");
    let uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
    let others = [(
        Url::parse("file:///pkg/src/types.ids").unwrap(),
        TYPES.to_string(),
    )];
    let at = crate::util::offset_to_position(&finished, source.len());
    let labels: Vec<_> = get_completions_with_project(&finished, &uri, at, &others)
        .into_iter()
        .filter(|c| c.label == "Message")
        .map(|c| c.detail)
        .collect();
    assert_eq!(labels.len(), 1, "only the local `Message`: {labels:?}");
}
fn package_with_dependency(active_uri: &str) -> Vec<crate::analysis::source::SourceFile> {
    use crate::analysis::source::SourceFile;
    let _ = active_uri;
    vec![
        SourceFile::local(
            Url::parse("file:///pkg/config.idp").unwrap(),
            "congregation app\nspecification_version = 1\n\ndependencies = {\n    shared = {\n        path = \"../shared\"\n    }\n}\n".to_string(),
        ),
        SourceFile::local(Url::parse("file:///pkg/src/types.ids").unwrap(), "struct User {\n    id: u64\n}\n".to_string()),
        SourceFile::of_dependency(
            Url::parse("file:///shared/src/models.ids").unwrap(),
            "/// A shared thing\nstruct Thing {\n    id: u64\n}\n".to_string(),
            "shared",
            vec!["shared".to_string(), "models".to_string()],
        ),
    ]
}
#[test]
fn dependency_types_are_offered_after_the_packages_own() {
    let source = "struct S {\n    m: ";
    let uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
    let others = package_with_dependency("file:///pkg/src/chat.ids");
    let end = crate::util::offset_to_position(source, source.len());
    let items = get_completions_with_project(source, &uri, end, &others);
    let user = items
        .iter()
        .find(|c| c.label == "User")
        .expect("the package's own type");
    let thing = items
        .iter()
        .find(|c| c.label == "Thing")
        .expect("the dependency's type");
    assert_eq!(user.sort_text.as_deref(), Some("~User"));
    assert_eq!(thing.sort_text.as_deref(), Some("~~Thing"));
    assert_eq!(
        thing.detail.as_deref(),
        Some("from `models.ids` (dependency `shared`) — adds `use shared::models::Thing`")
    );
    assert_eq!(
        thing.additional_text_edits.as_ref().unwrap()[0].new_text,
        "use shared::models::Thing\n\n"
    );
}

#[test]
fn a_deprecated_keyword_or_primitive_is_tagged_in_completions() {
    use comline_core::diagnostics::DiagnosticCode;
    use comline_core::edition::Edition;
    static DEPRECATED: vocabulary::Deprecation = vocabulary::Deprecation {
        word: "u8",
        since: Edition::E1,
        removed: None,
        replacement: Some("byte"),
        code: DiagnosticCode::warning("deprecation::u8"),
        note: "`byte` says what it is",
    };
    let lookup = |w: &str| (w == "u8").then_some(&DEPRECATED);

    let items = mark_deprecated_words(get_primitive_type_completions(), lookup);
    let u8 = items.iter().find(|i| i.label == "u8").unwrap();
    assert_eq!(
        u8.tags,
        Some(vec![lsp_types::CompletionItemTag::DEPRECATED])
    );
    assert!(u8
        .detail
        .as_deref()
        .unwrap()
        .contains("deprecated since edition 1"));
    let u16 = items.iter().find(|i| i.label == "u16").unwrap();
    assert_eq!(u16.tags, None);

    // Only a word of the language: a field or type of the same name isn't.
    let named = CompletionItem {
        label: "u8".to_string(),
        kind: Some(CompletionItemKind::FIELD),
        ..Default::default()
    };
    assert_eq!(mark_deprecated_words(vec![named], lookup)[0].tags, None);
}

#[test]
fn the_language_deprecates_no_completion_yet() {
    let items = get_completions(
        "",
        &Url::parse("file:///a.ids").unwrap(),
        Position::new(0, 0),
    );
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i.tags.is_none()));
}
