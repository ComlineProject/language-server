//! What this build is: the server's version and the `comline-core` commit it
//! links. Printed by `comline-lsp --version` and returned as
//! `serverInfo.version` in the `initialize` result, in the same form, so a
//! client reads the core commit from either with one pattern
//! (`core ([0-9a-f]{40})`).

/// The `comline-core` commit this build links, 40 hex digits, or `unknown`
/// (`build.rs`).
pub const CORE_REV: &str = env!("COMLINE_CORE_REV");

/// `0.1.0 (core 14ce54658455206d639ddc4fbe6d609d20292587)`.
pub fn server_version() -> String {
    format!("{} (core {CORE_REV})", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commit in the manifest's `comline-core` line, which a build with no
    /// lock reads and a build with one agrees with.
    fn manifest_rev() -> String {
        let manifest = include_str!("../Cargo.toml");
        let line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("comline-core"))
            .expect("a comline-core dependency");
        line.split("rev = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a rev on it")
            .to_string()
    }

    #[test]
    fn core_rev_is_the_pinned_commit() {
        assert_eq!(CORE_REV.len(), 40, "{CORE_REV}");
        assert!(
            CORE_REV.bytes().all(|b| b.is_ascii_hexdigit()),
            "{CORE_REV}"
        );
        assert!(CORE_REV.starts_with(&manifest_rev()), "{CORE_REV}");
    }

    #[test]
    fn version_carries_the_core_rev_in_one_pattern() {
        let version = server_version();
        assert_eq!(
            version,
            format!("{} (core {CORE_REV})", env!("CARGO_PKG_VERSION"))
        );
        assert!(version.ends_with(&format!("(core {CORE_REV})")));
    }
}
