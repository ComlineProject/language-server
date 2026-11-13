use super::*;
    use crate::test_support::{hover_at, hover_text, test_uri};
    use crate::util::offset_to_position;

    #[test]
    fn test_hover_on_struct() {
        let source = r#"
struct User {
    name: string
    age: i32
}
"#;
        // Hover over "User" on line 1
        let hover = hover_at(source, 1, 8);
        assert!(hover.is_some());

        let hover = hover.unwrap();
        if let HoverContents::Array(contents) = hover.contents {
            assert!(!contents.is_empty());
        }
    }

    #[test]
    fn test_hover_shows_fixed_struct_size() {
        let source = "struct Point {\n    x: u32\n    y: u32\n}\n";
        // Hover over "Point" on line 0.
        let hover = hover_at(source, 0, 8).expect("hover should resolve");
        let text = hover_text(hover);
        // The package's own format first, then the other official ones.
        assert!(text.contains("packed@1: 8 B"), "got: {text}");
        let packed = text.find("packed@1:").expect("packed line");
        let json = text.find("json@1:").expect("json line");
        let msgpack = text.find("msgpack@1:").expect("msgpack line");
        assert!(packed < json && packed < msgpack, "got: {text}");
        // Each field's own, in the package's format.
        assert!(text.contains("- #0 x: 4 B"), "got: {text}");
    }

    #[test]
    fn test_hover_shows_variable_size_for_a_string_field() {
        let source = "struct Note {\n    body: string\n}\n";
        // Hover over "Note" on line 0.
        let hover = hover_at(source, 0, 7).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("packed@1: 1 B\u{2013}unbounded"), "got: {text}");
    }

    #[test]
    fn test_hover_shows_variable_size_for_an_optional_field() {
        let source = "struct Thing {\n    optional id: u64\n}\n";
        // Hover over "Thing" on line 0.
        let hover = hover_at(source, 0, 8).expect("hover should resolve");
        let text = hover_text(hover);
        // Absent (a tag byte), or the tag and the `u64`.
        assert!(text.contains("packed@1: 1\u{2013}9 B"), "got: {text}");
    }

    #[test]
    fn test_struct_hover_shows_field_indices() {
        let source =
            "struct Greeting {\n    message: string\n    language: string\n    test: bool\n}\n";
        // Hover over "Greeting" on line 0.
        let hover = hover_at(source, 0, 8).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("#0 message"), "got: {text}");
        assert!(text.contains("#1 language"), "got: {text}");
        assert!(text.contains("#2 test"), "got: {text}");
    }

    #[test]
    fn a_bounded_field_shows_its_maximum_as_written() {
        let source =
            "struct Order {\n    sku: char[8]\n    note: char[max 280]\n    tags: u32[max 10]\n}\n";
        let uri = test_uri();
        for (line, written) in [
            (1, "sku: char[8]"),
            (2, "note: char[max 280]"),
            (3, "tags: u32[max 10]"),
        ] {
            let hover =
                get_hover_info(source, &uri, Position::new(line, 6)).expect("hover should resolve");
            let text = hover_text(hover);
            assert!(text.contains(written), "got: {text}");
        }
    }

    #[test]
    fn test_hover_on_a_field_name_shows_its_own_info() {
        let source =
            "struct Greeting {\n    message: string\n    language: string\n    test: bool\n}\n";
        // Hover over "language" (the field name) on line 2.
        let hover = hover_at(source, 2, 6).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("language: string"), "got: {text}");
        assert!(text.contains("#1"), "got: {text}");
        assert!(text.contains("of `Greeting`"), "got: {text}");
        assert!(text.contains("3 fields"), "got: {text}");
        assert!(text.contains("optional: no"), "got: {text}");
        // `string` is unbounded.
        assert!(text.contains("packed@1: 1 B\u{2013}unbounded"), "got: {text}");
    }

    #[test]
    fn a_one_byte_field_says_1_byte_not_1_bytes() {
        let source =
            "struct Greeting {\n    message: string\n    language: string\n    test: bool\n}\n";
        // Hover over "test" (the `bool` field, 1 byte wide) on line 3.
        let hover = hover_at(source, 3, 6).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("packed@1: 1 B"), "got: {text}");
    }

    #[test]
    fn a_one_byte_struct_says_1_byte_not_1_bytes() {
        let source = "struct Flag {\n    on: bool\n}\n";
        // Hover over "Flag" on line 0.
        let hover = hover_at(source, 0, 8).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("packed@1: 1 B"), "got: {text}");
    }

    #[test]
    fn hover_on_timeout_ms_annotation_shows_its_description() {
        let source = "protocol Thing {\n    @timeout_ms = 100\n    function foo();\n}\n";
        // Hover over "timeout_ms" on line 1.
        let hover = hover_at(source, 1, 7).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@timeout_ms"), "got: {text}");
        assert!(text.contains("waits"), "got: {text}");
        assert!(text.contains("milliseconds"), "got: {text}");
        assert!(text.contains("comline-rust"), "got: {text}");
    }

    #[test]
    fn hover_on_framing_annotation_shows_its_default() {
        let source = "@framing = \"jsonrpc\"\nprotocol Thing {\n}\n";
        let hover = hover_at(source, 0, 3).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@framing"), "got: {text}");
        assert!(text.contains("datagram@2"), "got: {text}");
    }

    #[test]
    fn hover_on_validators_annotation_shows_its_description() {
        let source = "struct X {\n    @validators = [Foo()]\n    name: string\n}\n";
        let hover = hover_at(source, 1, 7).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@validators"), "got: {text}");
        assert!(text.contains("validator"), "got: {text}");
    }

    #[test]
    fn hover_on_idempotent_annotation_says_not_consumed_yet() {
        let source = "protocol P {\n    @idempotent\n    function f();\n}\n";
        let hover = hover_at(source, 1, 8).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@idempotent"), "got: {text}");
        assert!(text.contains("not consumed anywhere yet"), "got: {text}");
    }

    #[test]
    fn hover_on_an_unrecognised_annotation_still_shows_something() {
        let source = "protocol P {\n    @custom_key = 1\n    function f();\n}\n";
        let hover = hover_at(source, 1, 8).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@custom_key"), "got: {text}");
        assert!(text.contains("open namespace"), "got: {text}");
    }

    #[test]
    fn hover_on_an_annotation_key_inside_a_comment_shows_nothing() {
        // `//@settings = Test` is a comment, not a real `@settings`
        // annotation - must not come back as "not a recognised annotation"
        // (or any hover at all).
        let source = "//@settings = Test\nstruct Greeting {\n    test: bool\n}\n";
        let uri = test_uri();
        let position = Position::new(0, 4);

        assert!(get_hover_info(source, &uri, position).is_none());
    }

    #[test]
    fn a_declaration_commented_out_with_a_block_comment_parses_and_hovers_as_the_real_one() {
        // The end-to-end version of the block-comment grammar fix: with a
        // `/* ... */`-commented-out `error Test { ... }` sitting above it,
        // the document must still parse as a whole (so symbol-table-backed
        // hover works at all) and the commented-out `Test` must not shadow
        // or interfere with the real `Greeting` declaration below it.
        let source =
            "/*\nerror Test {\n    message = \"\"\n}\n*/\nstruct Greeting {\n    name: string\n}\n";
        let uri = test_uri();
        let position = Position::new(5, 10); // "Greeting"

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("struct Greeting"), "got: {text}");
    }

    #[test]
    fn hover_on_a_field_named_like_an_annotation_key_gets_field_hover_not_annotation_hover() {
        // No `@` prefix — "timeout_ms" here is an ordinary field name, not
        // the annotation. Disambiguated by the preceding-`@` check, not by
        // the word's text.
        let source = "struct X {\n    timeout_ms: u32\n}\n";
        let hover = hover_at(source, 1, 6).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("timeout_ms: u32"), "got: {text}");
        assert!(!text.contains("waits indefinitely"), "got: {text}");
    }

    #[test]
    fn test_hover_on_an_optional_field_shows_optional_and_default() {
        let source = "struct Thing {\n    optional id: u64 = 0\n}\n";
        // Hover over "id" (the field name) on line 1.
        let hover = hover_at(source, 1, 14).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("optional id: u64 = 0"), "got: {text}");
        assert!(text.contains("optional: yes"), "got: {text}");
        // Optional is absent or the `u64`: the field's own range, not the
        // type's (`range_of_field`).
        assert!(text.contains("packed@1: 1\u{2013}9 B"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_field_shows_its_annotations() {
        let source = "struct Message {\n    @timeout_ms=1000\n    body: string\n}\n";
        // Hover over "body" (the field name) on line 2.
        let hover = hover_at(source, 2, 6).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("@timeout_ms=1000"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_field_in_an_error_shows_its_own_info() {
        let source = "error Rejected {\n    message = \"no\"\n    reason: string\n}\n";
        // Hover over "reason" (the field name) on line 2.
        let hover = hover_at(source, 2, 6).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("reason: string"), "got: {text}");
        assert!(text.contains("of `Rejected`"), "got: {text}");
    }

    #[test]
    fn test_hover_on_enum() {
        let source = r#"
enum Status {
    Active
    Inactive
}
"#;
        let hover = hover_at(source, 1, 6);
        assert!(hover.is_some());
    }

    #[test]
    fn test_hover_on_type() {
        let source = r#"
struct User {
    name: string
}
"#;
        // Hover over "string" type
        let hover = hover_at(source, 2, 11);
        assert!(hover.is_some());
    }

    /// A schema declaring a struct and a settings preset under one name.
    /// Core allows this - a settings block generates no type - so the
    /// editor has to tell them apart.
    const SHARED_NAME: &str = "struct Test {\n    a: string\n}\n\n\
                               settings Test {\n    validators.allowed = true\n}\n";

    #[test]
    fn hover_on_a_declaration_shows_that_declaration_not_a_namesake() {
        // The reported bug: hovering `struct Test` showed the tooltip for
        // `settings Test`, because the symbol table was keyed by name and
        // the settings block was declared last.
        let uri = test_uri();
        let offset = SHARED_NAME.find("struct Test").unwrap() + "struct ".len();
        let position = offset_to_position(SHARED_NAME, offset);

        let hover = get_hover_info(SHARED_NAME, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("struct"), "got: {text}");
        assert!(
            !text.contains("validators.allowed"),
            "settings block's tooltip: {text}"
        );
    }

    #[test]
    fn hover_on_a_settings_preset_still_shows_the_settings_block() {
        let uri = test_uri();
        let offset = SHARED_NAME.find("settings Test").unwrap() + "settings ".len();
        let position = offset_to_position(SHARED_NAME, offset);

        let hover = get_hover_info(SHARED_NAME, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("validators.allowed"), "got: {text}");
    }

    #[test]
    fn a_type_reference_resolves_to_the_declaration_not_the_preset() {
        let source = "struct Test {\n    a: string\n}\n\n\
                      settings Test {\n    validators.allowed = true\n}\n\n\
                      struct Holder {\n    t: Test\n}\n";
        let uri = test_uri();
        let offset = source.find("t: Test").unwrap() + "t: ".len();
        let position = offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("struct"), "got: {text}");
        assert!(!text.contains("validators.allowed"), "got: {text}");
    }

    #[test]
    fn test_hover_on_struct_declared_in_another_file() {
        // `chat.ids` references `Message`, declared only in `types.ids`.
        let chat_source = r#"use types::Message
protocol Chat {
    function send(text: string) -> Message;
}
"#;
        let types_source = "struct Message {\n    text: string\n    seq: u64\n}\n";

        let chat_uri = Url::parse("file:///chat.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // Hover over "Message" in the return-type position.
        let position = Position::new(2, 38);

        let hover = get_hover_info_with_project(
            chat_source,
            &chat_uri,
            position,
            &[(types_uri, types_source.to_string())],
        );

        let hover = hover.expect("cross-file struct hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("struct Message"), "got: {text}");
        assert!(text.contains("text"), "got: {text}");
        assert!(text.contains("seq"), "got: {text}");
        // `text: string` is unbounded, so the whole struct's size is too —
        // the direct regression guard for the `other_docs`-retention
        // restructuring that threads a cross-file SizeLookup through.
        assert!(text.contains("B\u{2013}unbounded"), "got: {text}");
    }

    /// Without a `use`, a name another file declares is unknown here: the
    /// hover doesn't describe a declaration nothing brings into scope.
    #[test]
    fn test_hover_does_not_reach_an_unimported_sibling() {
        let chat_source = "protocol Chat {\n    function send(text: string) -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let hover = get_hover_info_with_project(
            chat_source,
            &Url::parse("file:///chat.ids").unwrap(),
            Position::new(1, 37),
            &[(
                Url::parse("file:///types.ids").unwrap(),
                types_source.to_string(),
            )],
        );

        let text = hover.map(hover_text).unwrap_or_default();
        assert!(!text.contains("struct Message"), "got: {text}");
    }

    /// A field typed with a name nothing imports has no size to add: the
    /// struct has no size, not the unimported declaration's.
    #[test]
    fn a_size_does_not_count_an_unimported_type() {
        let types = [(
            Url::parse("file:///types.ids").unwrap(),
            "struct Message {\n    seq: u64\n}\n".to_string(),
        )];
        let uri = Url::parse("file:///chat.ids").unwrap();
        let hover_on = |source: &str, line: u32| {
            get_hover_info_with_project(source, &uri, Position::new(line, 8), &types)
                .map(hover_text)
                .unwrap_or_default()
        };

        let text = hover_on("struct S {\n    m: Message\n}\n", 0);
        assert!(!text.contains("packed@1"), "got: {text}");
        assert!(!text.contains("json@1"), "got: {text}");

        let text = hover_on("use types::Message\n\nstruct S {\n    m: Message\n}\n", 2);
        assert!(text.contains("packed@1: 8 B"), "got: {text}");
    }

    /// The package's own format comes first, whichever it is, and the
    /// others follow once each.
    #[test]
    fn the_packages_own_format_is_shown_first() {
        let names =
            |own: &str| -> Vec<&str> { size_formats(own).into_iter().map(|f| f.name).collect() };
        let default = names("packed@1");
        assert_eq!(default[0], "packed@1");
        assert!(default.contains(&"json@1") && default.contains(&"msgpack@1"));

        let json = names("json@1");
        assert_eq!(json[0], "json@1");
        assert_eq!(json.len(), default.len());

        // Not an official format, so no rules: the official ones, in order.
        assert_eq!(names("custom@1"), names("packed@1")[..].to_vec());
    }

    #[test]
    fn a_hover_leads_with_the_format_the_manifest_declares() {
        let manifest = (
            Url::parse("file:///pkg/config.idp").unwrap(),
            "congregation app\nspecification_version = 1\nwire = { format = \"json@1\" }\n".to_string(),
        );
        let uri = Url::parse("file:///pkg/src/a.ids").unwrap();
        let text = get_hover_info_with_project(
            "struct S {\n    on: bool\n}\n",
            &uri,
            Position::new(0, 8),
            &[manifest],
        )
        .map(hover_text)
        .expect("hover should resolve");
        let json = text.find("json@1:").expect("json line");
        let packed = text.find("packed@1:").expect("packed line");
        assert!(json < packed, "got: {text}");
    }

    #[test]
    fn a_range_reads_as_bytes() {
        let range = |min, max, upper_bound| Range {
            min,
            max,
            upper_bound,
        };
        assert_eq!(render_range(range(8, Some(8), false)), "8 B");
        assert_eq!(render_range(range(9, Some(137), false)), "9\u{2013}137 B");
        assert_eq!(render_range(range(9, None, false)), "9 B\u{2013}unbounded");
        assert_eq!(render_range(range(9, Some(137), true)), "9 B, at most 137 B");
    }

    /// An enum is sized in each format by itself; a `const` by its type.
    #[test]
    fn an_enum_and_a_constant_show_their_sizes() {
        let uri = test_uri();
        let hover = |source: &str, line, character| {
            get_hover_info(source, &uri, Position::new(line, character))
                .map(hover_text)
                .unwrap_or_default()
        };
        let text = hover("enum Color {\n    Red\n    Green\n}\n", 0, 6);
        assert!(text.contains("packed@1: 1 B"), "got: {text}");

        let text = hover("const LIMIT: u32 = 10\n", 0, 7);
        assert!(text.contains("packed@1: 4 B"), "got: {text}");
    }

    /// A std validator nothing imports: no signature, no docs.
    #[test]
    fn a_std_validator_hovers_only_once_imported() {
        let std = crate::analysis::stdlib::files(&crate::analysis::stdlib::root());
        let uri = Url::parse("file:///pkg/src/none.ids").unwrap();
        let body = "@validators = [StringBounds()]\ntype FixedNum = u8[2]\n";
        let hover_on = |source: &str, line: u32| {
            get_hover_info_with_project(source, &uri, Position::new(line, 18), &std)
                .map(hover_text)
                .unwrap_or_default()
        };

        let text = hover_on(body, 0);
        assert!(!text.contains("validator StringBounds"), "got: {text}");

        let imported = format!("use std::validators::StringBounds\n\n{body}");
        let text = hover_on(&imported, 2);
        assert!(text.contains("validator StringBounds"), "got: {text}");
    }

    #[test]
    fn test_hover_on_a_cross_file_cycle_does_not_hang() {
        // `A` (the active file) refers to `B`, declared only in a second
        // file, which refers back to `A` — nothing else in the codebase
        // guards this shape (comline-core's own cycle check is scoped to
        // one file's units). This must resolve to *something* sane, not
        // hang or panic.
        let a_source = "struct A {\n    b: B\n}\n";
        let b_source = "struct B {\n    a: A\n}\n";

        let a_uri = Url::parse("file:///a.ids").unwrap();
        let b_uri = Url::parse("file:///b.ids").unwrap();

        // Hover over "A" in its own declaration.
        let position = Position::new(0, 8);

        let hover = get_hover_info_with_project(
            a_source,
            &a_uri,
            position,
            &[(b_uri, b_source.to_string())],
        );

        let hover = hover.expect("hover should resolve even through a cross-file cycle");
        let text = hover_text(hover);
        // Sized in no format: nothing said, not a number.
        assert!(text.contains("struct A"), "got: {text}");
        assert!(!text.contains("packed@1"), "got: {text}");
    }

    #[test]
    fn test_hover_shows_docstring() {
        let source = "/// Hello\nstruct Message {\n    text: string\n}\n";
        // Hover over "Message" in the declaration itself.
        let hover = hover_at(source, 1, 8).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("Hello"), "got: {text}");
    }

    #[test]
    fn test_hover_on_signed_int_keyword() {
        let source = "struct Sample {\n    value: s16\n}\n";
        // Hover over "s16" on line 1.
        let hover = hover_at(source, 1, 12).expect("hover should resolve");
        let text = hover_text(hover);
        // Now the specific vocabulary description, not a generic category
        // — "16-bit signed integer" rather than "integer type".
        assert!(text.contains("16-bit signed integer"), "got: {text}");
    }

    #[test]
    fn test_hover_on_type_alias_shows_signature() {
        let source = "type UserId = u64\n";
        // Hover over "UserId" in the declaration itself.
        let hover = hover_at(source, 0, 7).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(
            text.contains("type UserId") && text.contains("u64"),
            "got: {text}"
        );
    }

    #[test]
    fn hover_on_a_type_alias_shows_its_validators() {
        let source = "@validators = [Bounds(max = 8)]\ntype Handle = string\n";
        let hover = hover_at(source, 1, 7).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(
            text.contains("@validators = [Bounds(max = 8)]\ntype Handle = string"),
            "got: {text}"
        );
    }

    #[test]
    fn test_hover_on_type_alias_chain_shows_fully_resolved_type() {
        let source = "type A = B\ntype B = u32\nstruct X {\n    id: A\n}\n";
        // Hover over "A" in its own declaration ("type A = B", "A" at
        // column 5).
        let hover = hover_at(source, 0, 5).expect("hover should resolve");
        let text = hover_text(hover);
        // The one-level signature names "B"; the fully-resolved chain
        // should also surface "u32" since it differs from the written
        // target.
        assert!(text.contains("type A = B"), "got: {text}");
        assert!(text.contains("u32"), "got: {text}");
    }

    #[test]
    fn use_types_message_resolves_silently_no_note() {
        let chat_source =
            "use types::Message\n\nprotocol Chat {\n    function send() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let chat_uri = Url::parse("file:///chat.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();

        // Hover over "Message" in the return-type position.
        let position = Position::new(3, 24);

        let hover = get_hover_info_with_project(
            chat_source,
            &chat_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct Message"), "got: {text}");
    }

    #[test]
    fn two_siblings_declaring_the_same_name_use_picks_the_imported_one() {
        // The actual bug item 4 of this whole plan exists to fix: a flat,
        // first-match scan can't tell `a.ids`'s `Message` from `b.ids`'s -
        // it just returns whichever file happened to come first in
        // `other_files`. `use b::Message` must resolve to `b`'s.
        let active_source = "use b::Message\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let a_source = "struct Message {\n    from_a: bool\n}\n";
        let b_source = "struct Message {\n    from_b: bool\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let a_uri = Url::parse("file:///a.ids").unwrap();
        let b_uri = Url::parse("file:///b.ids").unwrap();

        // Hover over "Message" in the return-type position. `a.ids` comes
        // *before* `b.ids` in `other_files` - a flat scan would find it
        // first and get this wrong.
        let position = Position::new(3, 20);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(a_uri, a_source.to_string()), (b_uri, b_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("from_b"), "got: {text}");
        assert!(!text.contains("from_a"), "got: {text}");
    }

    #[test]
    fn use_multi_resolves_a_listed_item_silently() {
        let active_source =
            "use types::{Message, Other}\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\nstruct Other {\n    n: u8\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();
        let position = Position::new(3, 24);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct Message"), "got: {text}");
    }

    #[test]
    fn use_glob_resolves_anything_in_the_namespace_silently() {
        let active_source = "use types::*\n\nprotocol P {\n    function f() -> Message;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();
        let position = Position::new(3, 24);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("use-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        assert!(text.contains("struct Message"), "got: {text}");
    }

    #[test]
    fn use_as_alias_resolves_the_aliased_name_to_the_real_declaration() {
        let active_source =
            "use types::Message as Msg\n\nprotocol P {\n    function f() -> Msg;\n}\n";
        let types_source = "struct Message {\n    text: string\n}\n";

        let active_uri = Url::parse("file:///active.ids").unwrap();
        let types_uri = Url::parse("file:///types.ids").unwrap();
        // Hover over "Msg" (the alias) in the return-type position.
        let position = Position::new(3, 20);

        let hover = get_hover_info_with_project(
            active_source,
            &active_uri,
            position,
            &[(types_uri, types_source.to_string())],
        )
        .expect("alias-resolved cross-file struct hover should resolve");
        let text = hover_text(hover);

        // The real declaration is `struct Message`, not `struct Msg` -
        // `types.ids`'s own symbol table never heard of "Msg".
        assert!(text.contains("struct Message"), "got: {text}");
    }

    #[test]
    fn a_std_type_hovers_with_its_std_declaration() {
        let active_source = "use std::http::Request\n\nstruct S {\n    r: Request\n}\n";
        let active_uri = Url::parse("file:///pkg/src/active.ids").unwrap();
        // Hover over "Request" in the field-type position.
        let position = Position::new(3, 9);
        let std = crate::analysis::stdlib::files(&crate::analysis::stdlib::root());

        let hover = get_hover_info_with_project(active_source, &active_uri, position, &std)
            .expect("std resolves");
        let text = hover_text(hover);

        assert!(text.contains("struct Request"), "got: {text}");
        assert!(
            text.contains("An HTTP request"),
            "std's docstring, got: {text}"
        );
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
    fn hover_on_a_type_from_a_dependency() {
        let chat = "use shared::models::Thing\n\nstruct S {\n    t: Thing\n}\n";
        let chat_uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let others = package_with_dependency("file:///pkg/src/chat.ids");

        let hover = get_hover_info_with_project(chat, &chat_uri, Position::new(3, 8), &others)
            .expect("a `use`-resolved type from a dependency hovers");
        let text = hover_text(hover);
        assert!(text.contains("struct Thing"), "got: {text}");
        assert!(text.contains("A shared thing"), "got: {text}");
        assert!(!text.contains("no `use` here"), "it is imported: {text}");
    }

    // ---- module docs on path segments ----

    use crate::analysis::source::SourceFile;

    fn local(path: &str, text: &str) -> SourceFile {
        SourceFile::local(
            Url::parse(&format!("file:///pkg/{path}")).unwrap(),
            text.to_string(),
        )
    }

    /// The files of a package with documented modules, and std.
    fn documented_package() -> Vec<SourceFile> {
        let mut files = vec![
            local("src/types.ids", "//! The package's types.\n//!\n//! Shared by everything.\n\n/// Someone.\nstruct User {\n    id: u64\n}\n"),
            local("src/api.ids", "//! The API.\n\nstruct Root {\n    id: u64\n}\n"),
            local("src/api/common.ids", "//! Things every API schema needs.\n\nstruct Error {\n    code: u32\n}\n"),
        ];
        files.extend(crate::analysis::stdlib::files(
            &crate::analysis::stdlib::root(),
        ));
        files
    }

    /// The hover at the first `needle` in `source` (the cursor on its first
    /// character + `shift`), written in `pkg/src/<file>`.
    fn hover_in(file: &str, source: &str, needle: &str, shift: usize) -> Option<String> {
        let uri = Url::parse(&format!("file:///pkg/src/{file}")).unwrap();
        let offset = source.find(needle).unwrap() + shift;
        let position = crate::util::offset_to_position(source, offset);
        get_hover_info_with_project(source, &uri, position, &documented_package()).map(hover_text)
    }

    #[test]
    fn a_module_segment_shows_its_docs_and_the_types_it_declares() {
        let text = hover_in("chat.ids", "use types::User\n", "types", 1).expect("module hover");
        assert!(text.contains("module types"), "{text}");
        assert!(
            text.contains("The package's types.\n\nShared by everything."),
            "{text}"
        );
        assert!(text.contains("**Declares**"), "{text}");
        assert!(text.contains("- `struct User` — Someone."), "{text}");
    }

    #[test]
    fn std_and_each_of_its_modules_show_their_own_docs() {
        let source = "use std::validators::StringBounds\n";

        let package = hover_in("chat.ids", source, "std", 0).unwrap();
        assert!(package.contains("package std"), "{package}");
        assert!(
            package.contains("The Comline standard library."),
            "its config.idp docs: {package}"
        );
        assert!(package.contains("**Modules**"), "{package}");
        assert!(
            package.contains(
                "- `http` — Types for talking HTTP: request methods, requests and responses."
            ),
            "{package}"
        );
        assert!(
            package.contains("- `validators` — Validators to attach to fields with `@validators`."),
            "{package}"
        );

        let module = hover_in("chat.ids", source, "validators", 0).unwrap();
        assert!(module.contains("module std::validators"), "{module}");
        assert!(
            module.contains("Validators to attach to fields with `@validators`."),
            "{module}"
        );
        assert!(module.contains("- `validator StringBounds` — Checks a string's length is within a minimum and a maximum."), "{module}");
        assert!(
            !module.contains("**Modules**"),
            "a schema has no modules below it: {module}"
        );
    }

    #[test]
    fn a_last_segment_that_names_a_declaration_keeps_its_own_hover() {
        let text = hover_in("chat.ids", "use std::http::Request\n", "Request", 0).unwrap();
        assert!(text.contains("struct Request"), "{text}");
        assert!(
            !text.contains("module"),
            "the declaration, not a module: {text}"
        );
    }

    #[test]
    fn a_whole_namespace_import_names_a_module_at_the_end() {
        let text = hover_in("chat.ids", "use std::http\n", "http", 0).unwrap();
        assert!(text.contains("module std::http"), "{text}");
        assert!(
            text.contains("- `enum HttpMethod` — An HTTP request method."),
            "{text}"
        );
        assert!(
            text.contains("- `struct Response` — An HTTP response, by its status code."),
            "{text}"
        );
        let lone = hover_in("chat.ids", "use types\n", "types", 0).unwrap();
        assert!(
            lone.contains("module types"),
            "a lone word on a `use` line: {lone}"
        );
    }

    #[test]
    fn a_directory_over_a_schema_lists_both() {
        let text = hover_in("chat.ids", "use api::common::Error\n", "api", 0).unwrap();
        assert!(text.contains("module api\n"), "{text}");
        assert!(text.contains("The API."), "{text}");
        assert!(
            text.contains("**Modules**\n\n- `common` — Things every API schema needs."),
            "{text}"
        );
        assert!(text.contains("- `struct Root`"), "{text}");
    }

    #[test]
    fn relative_prefixes_show_the_module_they_stand_for() {
        let text = hover_in("api/b.ids", "use parent::common::Error\n", "parent", 2).unwrap();
        assert!(
            text.contains("module api"),
            "one up from api::b is api: {text}"
        );
        assert!(text.contains("The API."), "{text}");
        let me = hover_in("types.ids", "use self::x::Y\n", "self", 0).unwrap_or_default();
        assert!(!me.contains("package"), "{me}");
    }

    #[test]
    fn qualified_type_names_work_too() {
        let source = "struct S {\n    r: std::http::Request\n}\n";
        let text = hover_in("chat.ids", source, "http", 0).unwrap();
        assert!(text.contains("module std::http"), "{text}");
        let package = hover_in("chat.ids", source, "std", 0).unwrap();
        assert!(package.contains("package std"), "{package}");
    }

    #[test]
    fn not_for_aliases_comments_strings_or_unknown_paths() {
        // An alias hovers as what it stands for, never as a module of that name.
        let alias = hover_in("chat.ids", "use std::http::Request as types\n", "types", 0).unwrap();
        assert!(
            alias.contains("struct Request") && !alias.contains("module"),
            "{alias}"
        );
        assert_eq!(
            hover_in(
                "chat.ids",
                "// see types::User\nstruct S {\n    id: u64\n}\n",
                "types",
                0
            ),
            None,
            "a comment"
        );
        assert_eq!(
            hover_in("chat.ids", "use typse::User\n", "typse", 0),
            None,
            "no such module"
        );
        assert_eq!(
            hover_in("chat.ids", "use std::htp::Request\n", "htp", 0),
            None,
            "no such std module"
        );
        assert_eq!(
            hover_in("chat.ids", "struct types {\n    id: u64\n}\n", "types", 0)
                .map(|t| t.contains("module")),
            Some(false)
        );
    }

    #[test]
    fn works_while_the_rest_of_the_file_does_not_parse() {
        let source = "use std::http::Request\n\nstruct Broken {\n";
        let text = hover_in("chat.ids", source, "http", 0).expect("only the path is needed");
        assert!(text.contains("module std::http"), "{text}");
    }

    // ---- hover on error/validator/settings references ----
    //
    // These three were entirely absent from the symbol table, so a
    // reference to one hovered with nothing at all - not even a bare name,
    // since it never reached find_type_at_position either (an annotation
    // value or a `! Name` throws clause isn't a `Type` position).

    #[test]
    fn hover_on_a_validator_reference_shows_its_parameters() {
        let source = "validator StringBounds for string {\n    min: u32\n    max: u32\n}\n\nstruct S {\n    @validators = [StringBounds(min=3, max=10)]\n    name: string\n}\n";
        let uri = test_uri();
        let offset = source.rfind("StringBounds").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(
            text.contains("validator StringBounds(min: u32, max: u32)"),
            "got: {text}"
        );
        assert!(text.contains("2 parameters"), "got: {text}");
    }

    #[test]
    fn hover_on_an_error_reference_shows_its_fields() {
        let source = "error NotFound {\n    message = \"missing\"\n    id: u64\n}\n\nprotocol P {\n    function f() ! NotFound;\n}\n";
        let uri = test_uri();
        let offset = source.rfind("NotFound").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("error NotFound"), "got: {text}");
        assert!(text.contains("id: u64"), "got: {text}");
        assert!(text.contains("1 fields"), "got: {text}");
    }

    #[test]
    fn hover_on_a_settings_declaration_shows_its_entries() {
        let source = "settings AAA {\n    k = true\n}\n";
        let uri = test_uri();
        let offset = source.find("AAA").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("settings AAA"), "got: {text}");
        assert!(text.contains("k = true"), "got: {text}");
    }

    #[test]
    fn hover_on_an_unnamed_settings_block_shows_its_entries() {
        // Direct regression guard: an unnamed block has no name to be a
        // symbol under, so the old symbol-table-only path returned no
        // hover at all here.
        let source = "settings {\n    k = true\n}\n";
        let uri = test_uri();
        let offset = source.find("settings").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("k = true"), "got: {text}");
        assert!(text.contains("recognized keys"), "got: {text}");
    }

    #[test]
    fn hover_on_a_settings_entry_shows_the_catalog_doc_and_its_raw_value() {
        let source = "settings {\n    validators.allowed = false\n}\n";
        let uri = test_uri();
        let offset = source.find("allowed").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("validators.allowed = false"), "got: {text}");
        assert!(text.contains("permitted"), "got: {text}");
        assert!(text.contains("effective"), "got: {text}");
    }

    #[test]
    fn hover_on_a_settings_entry_with_no_catalog_match_only_shows_raw_and_effective() {
        // A key that doesn't match any shape `settings::enforcement`
        // recognizes still gets raw + effective, just no catalog doc —
        // the catalog section should be conditional, not always-on noise.
        let source = "settings {\n    timeout_ms = 500\n}\n";
        let uri = test_uri();
        let offset = source.find("timeout_ms").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info(source, &uri, position).expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("timeout_ms = 500"), "got: {text}");
        assert!(text.contains("effective"), "got: {text}");
        assert!(!text.contains("permitted"), "got: {text}");
    }

    #[test]
    fn hover_on_a_named_preset_entry_notes_which_settings_name_it_applies_to() {
        // Also exercises the cross-file manifest-sibling wiring end to end
        // (parses a real `.idp`, extracts its settings) rather than just
        // defaulting to empty — a regression guard for that plumbing, even
        // though the preset's own written value always wins at its own
        // key regardless of what the package says (last-write-wins merge).
        let source = "settings Strict {\n    validators.StringBounds.allowed = false\n}\n";
        let manifest = "congregation acme\nspecification_version = 1\nsettings = {\n    annotations = {\n        framing = {\n            allowed = true\n        }\n    }\n}\n";
        let schema_uri = Url::parse("file:///test.ids").unwrap();
        let manifest_uri = Url::parse("file:///config.idp").unwrap();

        let offset = source.find("allowed").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info_with_project(
            source,
            &schema_uri,
            position,
            &[(manifest_uri, manifest.to_string())],
        );
        let hover = hover.expect("hover should resolve");
        let text = hover_text(hover);
        assert!(
            text.contains("validators.StringBounds.allowed = false"),
            "got: {text}"
        );
        assert!(text.contains("@settings = Strict"), "got: {text}");
    }

    #[test]
    fn settings_hover_does_not_crash_when_a_dependencys_manifest_is_also_a_cross_package_reference()
    {
        // The manifest lookup here used to be `.find(|f| is_manifest(..))`
        // with no dependency filter — just the first `.idp` in `other_files`,
        // which this now fixes (`f.dependency().is_none()`). Whichever
        // manifest it reads, it must never crash: the dependency's own
        // manifest here has a `settings` value that's itself a cross-package
        // reference (`declared_settings` used to panic on that shape with no
        // override available, before being made to recognize and skip it).
        use crate::analysis::source::SourceFile;

        let source = "settings {\n    max_depth = 2\n}\n";
        let local_manifest = "congregation app\nspecification_version = 1\n";
        let dep_manifest = "congregation shared\nspecification_version = 1\n\
            settings = other::settings::Whatever\n";
        let schema_uri = Url::parse("file:///pkg/src/main.ids").unwrap();
        let local_manifest_uri = Url::parse("file:///pkg/config.idp").unwrap();
        let dep_manifest_uri = Url::parse("file:///shared/config.idp").unwrap();

        let offset = source.find("max_depth").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let other_files = [
            SourceFile::of_dependency(
                dep_manifest_uri,
                dep_manifest.to_string(),
                "shared",
                vec!["shared".to_string()],
            ),
            SourceFile::local(local_manifest_uri, local_manifest.to_string()),
        ];

        let hover = get_hover_info_with_project(source, &schema_uri, position, &other_files)
            .expect("hover should resolve without panicking");
        let text = hover_text(hover);
        assert!(text.contains("max_depth = 2"), "got: {text}");
    }

    #[test]
    fn hover_on_a_std_validator_reference_shows_its_declaration() {
        let source = "use std::validators::StringBounds\n\nstruct S {\n    @validators = [StringBounds(min=3, max=10)]\n    name: string\n}\n";
        let uri = Url::parse("file:///pkg/src/chat.ids").unwrap();
        let offset = source.rfind("StringBounds").unwrap();
        let position = crate::util::offset_to_position(source, offset);

        let hover = get_hover_info_with_project(source, &uri, position, &documented_package())
            .expect("hover should resolve");
        let text = hover_text(hover);
        assert!(text.contains("validator StringBounds"), "got: {text}");
        assert!(
            text.contains("Checks a string's length is within a minimum and a maximum."),
            "got: {text}"
        );
    }