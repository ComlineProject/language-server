// Builder for the `@annotation` hover shape: a `@key` signature, a
// description, optional default/value lines (labels bolded, not the
// whole line — the label is the only part that needs to stand out), and
// a Notes section for anything advisory (e.g. "not consumed anywhere
// yet") or cautionary. The signature is a literal fence embedded in a
// markdown block, not `MarkedString::from_language_code` — the legacy
// LanguageString variant renders with noticeably less visual distinction
// than an inline ```comline fence (confirmed against the real widget via
// the ComlineProject/hover-sketch scratch tooling); see unit_hover.rs
// for the same choice made the same way.
use lsp_types::{Hover, HoverContents, MarkedString};

pub struct AnnotationHover {
    key: String,
    description: Option<String>,
    default: Option<String>,
    value: Option<String>,
    notes: Vec<String>,
}

impl AnnotationHover {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), description: None, default: None, value: None, notes: Vec::new() }
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn default(mut self, text: impl Into<String>) -> Self {
        self.default = Some(text.into());
        self
    }

    pub fn value(mut self, text: impl Into<String>) -> Self {
        self.value = Some(text.into());
        self
    }

    /// An informational line in the Notes section — e.g. "consumed by: X, Y".
    pub fn note(mut self, text: impl Into<String>) -> Self {
        self.notes.push(text.into());
        self
    }

    /// Like `note`, but prefixed with a warning glyph — the only
    /// available way to flag something as cautionary rather than merely
    /// informational, since color isn't an option (see unit_hover.rs's
    /// module comment).
    pub fn warning(mut self, text: impl Into<String>) -> Self {
        self.notes.push(format!("⚠️ {}", text.into()));
        self
    }

    pub fn build(self) -> Hover {
        let mut contents = vec![MarkedString::from_markdown(format!("```comline\n@{}\n```", self.key))];

        if let Some(description) = self.description {
            contents.push(MarkedString::from_markdown(description));
        }

        let mut details = Vec::new();
        if let Some(default) = &self.default {
            details.push(format!("- **default:** {default}"));
        }
        if let Some(value) = &self.value {
            details.push(format!("- **value:** {value}"));
        }
        if !details.is_empty() {
            contents.push(MarkedString::from_markdown(details.join("\n")));
        }

        if !self.notes.is_empty() {
            let mut block = vec!["#### Notes".to_string(), String::new()];
            block.extend(self.notes.iter().map(|n| format!("- {n}")));
            contents.push(MarkedString::from_markdown(block.join("\n")));
        }

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
    fn signature_is_a_fenced_block_not_a_language_string() {
        let hover = AnnotationHover::new("settings").build();
        let parts = blocks(hover);
        assert_eq!(parts, vec!["```comline\n@settings\n```".to_string()]);
    }

    #[test]
    fn default_and_value_labels_are_bolded_the_rest_of_the_line_is_not() {
        let hover = AnnotationHover::new("settings")
            .default("no preset applied")
            .value("a bare name")
            .build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[1], "- **default:** no preset applied\n- **value:** a bare name");
    }

    #[test]
    fn omitted_default_or_value_is_just_skipped() {
        let hover = AnnotationHover::new("settings").value("a bare name").build();
        let parts = blocks(hover);
        assert_eq!(parts[1], "- **value:** a bare name");
        assert!(!parts[1].contains("default"), "{}", parts[1]);
    }

    #[test]
    fn notes_get_their_own_section_as_a_separate_block() {
        let hover = AnnotationHover::new("settings").note("consumed by: enforcement").build();
        let parts = blocks(hover);
        let notes = parts.last().unwrap();
        assert!(notes.starts_with("#### Notes"), "{notes}");
        assert!(notes.contains("- consumed by: enforcement"), "{notes}");
    }

    #[test]
    fn a_warning_gets_a_glyph_prefix_in_the_same_notes_section() {
        let hover = AnnotationHover::new("settings").warning("not consumed anywhere yet").build();
        let parts = blocks(hover);
        let notes = parts.last().unwrap();
        assert!(notes.contains("- ⚠️ not consumed anywhere yet"), "{notes}");
    }

    #[test]
    fn no_notes_means_no_notes_block_at_all() {
        let hover = AnnotationHover::new("settings").description("does a thing").build();
        let parts = blocks(hover);
        assert!(!parts.iter().any(|p| p.contains("Notes")), "{parts:?}");
    }

    #[test]
    fn matches_the_current_at_settings_hover_content() {
        // Same facts `create_annotation_hover` in hover.rs renders today
        // (core's AnnotationInfo for "settings") — restructured per the
        // requested adjustments: fenced signature, bolded labels, and
        // "not consumed anywhere yet" moved into a dedicated Notes
        // section instead of a bare trailing sentence.
        let hover = AnnotationHover::new("settings")
            .description(
                "Opts a declaration into a named `settings NAME { ... }` preset declared \
                 in this schema, merged onto whatever settings already apply to it.",
            )
            .default("no preset applied — only the package/schema-level settings apply")
            .value("a bare name, e.g. `Strict` for a `settings Strict { ... }` block")
            .warning("not consumed anywhere yet — decided, advisory metadata only")
            .build();
        let parts = blocks(hover);
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert_eq!(parts[0], "```comline\n@settings\n```");
        assert_eq!(
            parts[2],
            "- **default:** no preset applied — only the package/schema-level settings apply\n\
             - **value:** a bare name, e.g. `Strict` for a `settings Strict { ... }` block"
        );
        assert_eq!(parts[3], "#### Notes\n\n- ⚠️ not consumed anywhere yet — decided, advisory metadata only");
    }
}
