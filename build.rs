//! Records the `comline-core` commit this build links, for `comline-lsp
//! --version` and the `initialize` result (`src/version.rs`).
//!
//! The commit comes from `Cargo.lock` (the `#<sha>` of the `comline-core`
//! git source), which is what the build resolved. The lock isn't committed
//! here, and a build that doesn't see one (`cargo install`, a git dependency)
//! falls back to the full `rev` in `Cargo.toml`, then to `unknown`.

use std::{env, fs, path::Path};

fn main() {
    let dir = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let dir = Path::new(&dir);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if dir.join("Cargo.lock").exists() {
        println!("cargo:rerun-if-changed=Cargo.lock");
    }

    let rev = dir
        .ancestors()
        .filter_map(|d| fs::read_to_string(d.join("Cargo.lock")).ok())
        .find_map(|lock| rev_from_lock(&lock))
        .or_else(|| {
            let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
            rev_from_manifest(&manifest)
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=COMLINE_CORE_REV={rev}");
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `source = "git+https://github.com/ComlineProject/core?rev=...#<sha>"` of
/// the `comline-core` package.
fn rev_from_lock(lock: &str) -> Option<String> {
    let mut package = "";
    for line in lock.lines() {
        if let Some(name) = line.strip_prefix("name = \"") {
            package = name.trim_end_matches('"');
        } else if package == "comline-core" {
            if let Some(source) = line.strip_prefix("source = \"git+") {
                let sha = source.rsplit('#').next()?.trim_end_matches('"');
                return is_sha(sha).then(|| sha.to_ascii_lowercase());
            }
        }
    }
    None
}

/// `comline-core = { git = "...", rev = "<sha>" }`.
fn rev_from_manifest(manifest: &str) -> Option<String> {
    let line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("comline-core"))?;
    let rest = line.split("rev = \"").nth(1)?;
    let sha = rest.split('"').next()?;
    is_sha(sha).then(|| sha.to_ascii_lowercase())
}
