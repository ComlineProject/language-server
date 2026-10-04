// Known `@key=value` annotations - the single source of truth for both
// completion (`handlers::completion`) and hover (`handlers::hover`), so the
// two can't silently drift on what a key means or what it defaults to.
//
// `@key=value` is an open namespace (see `design/core-target-contract.md`'s
// "Per-call settings" and "Transport, framing & format" sections) - any key
// parses and freezes regardless of whether it's listed here. This table is
// deliberately a curated, known-good subset: the keys a real generator or
// `core`'s own validation pass actually reads today, not an exhaustive or
// enforced list. An unrecognised key is not an error.

/// Which declaration an `@key=value` annotation attaches to. The grammar
/// permits annotations on a `struct`, a `Field`, a `protocol`, and a
/// `Function` (`core/src/schema/idl/grammar.rs`); `Leading` covers the
/// first two - a struct's and a protocol's own annotation sit in the same
/// "top level, right before the keyword" position, indistinguishable
/// without looking past the cursor at text that doesn't exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationScope {
    /// Top level, before `struct` or `protocol`.
    Leading,
    /// Inside a `struct`/`error` body, before a field.
    Field,
    /// Inside a `protocol` body, before a function.
    Function,
}

/// Everything known about one `@key`.
pub struct AnnotationInfo {
    pub key: &'static str,
    pub scope: AnnotationScope,
    /// One-line summary of what it does.
    pub description: &'static str,
    /// Plain-text behavior when the annotation is absent.
    pub default: &'static str,
    /// The expected value's shape, for display.
    pub value: &'static str,
    /// Where it's actually read today - `None` for a key that's decided
    /// but has no consumer yet (purely advisory metadata).
    pub consumed_by: Option<&'static str>,
}

pub const KNOWN_ANNOTATIONS: &[AnnotationInfo] = &[
    AnnotationInfo {
        key: "validators",
        scope: AnnotationScope::Field,
        description: "Attaches one or more named validators to this field.",
        default: "no validators run",
        value: "a list of validator calls, e.g. `[StringBounds(min_chars = 3)]`",
        consumed_by: Some("`comline build`'s validation pass"),
    },
    AnnotationInfo {
        key: "timeout_ms",
        scope: AnnotationScope::Function,
        description: "How long the client waits for the response before timing out. \
                       Request/response calls only — a one-way call has nothing to wait for.",
        default: "no timeout — waits indefinitely",
        value: "an integer, in milliseconds",
        consumed_by: Some("the generated Rust client (`comline-rust`)"),
    },
    AnnotationInfo {
        key: "idempotent",
        scope: AnnotationScope::Function,
        description: "Marks that calling this function twice has the same effect as calling \
                       it once — safe to retry.",
        default: "not idempotent",
        // Conceptually a bare marker (see `design/core-target-contract.md`'s
        // "Per-call settings"), but the grammar's `Annotation` rule
        // requires `=value` unconditionally and `Expression` has no
        // boolean variant — `@idempotent` alone does not parse today.
        // `= true` (a bare identifier, not a bool literal) is the
        // convention until the grammar grows one (`structure.md`'s own
        // `@internal=true` example uses the same workaround).
        value: "requires a value today (the grammar has no bare-marker form yet) — \
                conventionally `= true`",
        consumed_by: None,
    },
    AnnotationInfo {
        key: "framing",
        scope: AnnotationScope::Leading,
        description: "The wire framing the generated client/server use for this protocol. \
                       No effect on a struct.",
        default: "datagram — Comline's compact binary framing",
        value: "one of `\"jsonrpc\"` (`\"json-rpc\"`, `\"jsonrpc-2.0\"`), or `\"datagram\"` \
                to opt a single protocol back out of a package-wide default",
        consumed_by: Some("both `comline-rust` and `comline-typescript`'s generators"),
    },
];

/// Look up a known annotation by its key (the part right after `@`, no
/// `=value`). `None` for anything not in [`KNOWN_ANNOTATIONS`] — including
/// a perfectly valid, real annotation this table just doesn't know about
/// yet (open namespace; see the module doc).
pub fn lookup(key: &str) -> Option<&'static AnnotationInfo> {
    KNOWN_ANNOTATIONS.iter().find(|a| a.key == key)
}

/// Every known annotation valid in `scope`, in table order.
pub fn for_scope(scope: AnnotationScope) -> impl Iterator<Item = &'static AnnotationInfo> {
    KNOWN_ANNOTATIONS.iter().filter(move |a| a.scope == scope)
}
