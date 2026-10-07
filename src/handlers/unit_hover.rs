// Builder for the "typical unit" hover shape: a `value: type` signature,
// an optional description, an optional field list, and zero or more
// named examples. Every code portion renders as its own ```comline
// fence rather than attempting inline styling — VS Code's Hover
// sanitizer strips `style=` attributes entirely (confirmed empirically:
// see the ComlineProject/hover-sketch scratch tooling), but a fenced
// code block still runs through the real comline grammar and gets
// genuine theme-colored syntax highlighting, same as any other code
// block.
use lsp_types::{Hover, HoverContents, MarkedString};

pub struct UnitHover {
    signature: String,
    description: Option<String>,
    fields: Vec<(String, String)>,
    examples: Vec<(String, String)>,
    extra: Vec<String>,
}

impl UnitHover {
    pub fn new(signature: impl Into<String>) -> Self {
        Self {
            signature: signature.into(),
            description: None,
            fields: Vec::new(),
            examples: Vec::new(),
            extra: Vec::new(),
        }
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn field(mut self, name: impl Into<String>, ty: impl Into<String>) -> Self {
        self.fields.push((name.into(), ty.into()));
        self
    }

    pub fn example(mut self, name: impl Into<String>, code: impl Into<String>) -> Self {
        self.examples.push((name.into(), code.into()));
        self
    }

    /// An extra markdown block appended after the main body, as its own
    /// `Hover` array entry (its own visually-divided block) — e.g. an
    /// "effective value" line for settings hover. Raw markdown, not
    /// fenced; call multiple times for multiple trailing blocks.
    pub fn extra(mut self, markdown: impl Into<String>) -> Self {
        self.extra.push(markdown.into());
        self
    }

    pub fn build(self) -> Hover {
        let mut body = vec![format!("```comline\n{}\n```", self.signature)];

        if let Some(description) = &self.description {
            body.push(String::new());
            body.push("#### Description".to_string());
            body.push(String::new());
            body.push(description.clone());
        }

        if !self.fields.is_empty() {
            body.push(String::new());
            body.push("#### Fields".to_string());
            body.push(String::new());
            body.push("```comline".to_string());
            body.extend(self.fields.iter().map(|(name, ty)| format!("{name}: {ty}")));
            body.push("```".to_string());
        }

        if !self.examples.is_empty() {
            body.push(String::new());
            body.push("#### Examples".to_string());
            for (index, (name, code)) in self.examples.iter().enumerate() {
                let heading = if name.is_empty() { format!("Example {}", index + 1) } else { name.clone() };
                body.push(String::new());
                body.push(format!("##### {heading}"));
                body.push(String::new());
                body.push("```comline".to_string());
                body.push(code.clone());
                body.push("```".to_string());
            }
        }

        let mut contents = vec![MarkedString::from_markdown(body.join("\n"))];
        contents.extend(self.extra.into_iter().map(MarkedString::from_markdown));

        Hover { contents: HoverContents::Array(contents), range: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(hover: Hover) -> Vec<String> {
        match hover.contents {
            HoverContents::Array(parts) => parts
                .into_iter()
                .map(|p| match p {
                    MarkedString::String(s) => s,
                    MarkedString::LanguageString(ls) => ls.value,
                })
                .collect(),
            _ => panic!("expected an array of blocks"),
        }
    }

    #[test]
    fn signature_only_is_a_single_fenced_block() {
        let hover = UnitHover::new("value: type").build();
        let parts = blocks(hover);
        assert_eq!(parts, vec!["```comline\nvalue: type\n```".to_string()]);
    }

    #[test]
    fn description_adds_a_heading_and_body() {
        let hover = UnitHover::new("value: type").description("What this does.").build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("#### Description"));
        assert!(parts[0].contains("What this does."));
    }

    #[test]
    fn fields_render_as_their_own_fenced_block() {
        let hover = UnitHover::new("value: type").field("foo", "string").field("bar", "int").build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("#### Fields"));
        assert!(parts[0].contains("```comline\nfoo: string\nbar: int\n```"), "{}", parts[0]);
    }

    #[test]
    fn named_examples_each_get_their_own_heading_and_fence() {
        let hover = UnitHover::new("value: type")
            .example("Example 1", "foo.bar = true")
            .example("Example 2", "foo.bar = false")
            .build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("##### Example 1"), "{}", parts[0]);
        assert!(parts[0].contains("##### Example 2"), "{}", parts[0]);
        assert!(parts[0].contains("foo.bar = true"), "{}", parts[0]);
        assert!(parts[0].contains("foo.bar = false"), "{}", parts[0]);
    }

    #[test]
    fn an_unnamed_example_is_numbered() {
        let hover = UnitHover::new("value: type").example("", "foo.bar = true").build();
        let parts = blocks(hover);
        assert!(parts[0].contains("##### Example 1"), "{}", parts[0]);
    }

    #[test]
    fn extra_blocks_are_their_own_separate_array_entries() {
        let hover = UnitHover::new("value: type")
            .extra("`effective:` **Bool(true)**")
            .extra("a second trailing block")
            .build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[1], "`effective:` **Bool(true)**");
        assert_eq!(parts[2], "a second trailing block");
    }

    #[test]
    fn matches_the_typical_unit_shape_end_to_end() {
        // Mirrors tooltips/typical-unit.js from the hover-sketch scratch
        // tooling exactly, to keep this builder and that sketch from
        // drifting apart.
        let hover = UnitHover::new("value: type")
            .description("**Whether validators** in general **are permitted** on declarations this key's scope covers.")
            .field("foo", "string")
            .field("bar", "int")
            .example("Example 1", "foo.bar = true")
            .extra("`effective:` **Bool(true)** _(applies to declarations using `@settings = Test`)_")
            .build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(
            parts[0],
            [
                "```comline",
                "value: type",
                "```",
                "",
                "#### Description",
                "",
                "**Whether validators** in general **are permitted** on declarations this key's scope covers.",
                "",
                "#### Fields",
                "",
                "```comline",
                "foo: string",
                "bar: int",
                "```",
                "",
                "#### Examples",
                "",
                "##### Example 1",
                "",
                "```comline",
                "foo.bar = true",
                "```",
            ]
            .join("\n")
        );
        assert_eq!(parts[1], "`effective:` **Bool(true)** _(applies to declarations using `@settings = Test`)_");
    }
}
