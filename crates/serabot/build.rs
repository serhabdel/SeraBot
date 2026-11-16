//! Optional compile-time default for the Antigravity OAuth client secret.
//!
//! The value is Google's installed-application credential. GitHub push
//! protection rejects it as a literal in the repository, so it is read from the
//! git-ignored `config/agy.local.toml` (first line) when present and exposed as
//! `SERABOT_AGY_CLIENT_SECRET`. Without the file the default is empty and the
//! runtime sources (console field, `AGY_CLIENT_SECRET`) apply.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let file = root.join("../../config/agy.local.toml");
    println!("cargo:rerun-if-changed={}", file.display());
    println!("cargo:rerun-if-env-changed=SERABOT_AGY_CLIENT_SECRET");
    let from_file = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_owned()))
        .filter(|s| !s.is_empty());
    let value = std::env::var("SERABOT_AGY_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or(from_file)
        .unwrap_or_default();
    println!("cargo:rustc-env=SERABOT_AGY_CLIENT_SECRET={value}");
}
