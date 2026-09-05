// Embeds the repository CHANGELOG into the binary so `--whats-new` can print it
// with no network call. When building from the repository (CI release builds,
// `cargo build` in the tree) the real file is read; when building from the
// published crate tarball, where the repo-root file is absent, a short pointer
// to the online changelog is embedded instead.
use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=../../CHANGELOG.md");
    let fallback =
        "# Changelog\n\nThis build did not embed the changelog.\nSee https://github.com/kannandreams/agent-top/blob/main/CHANGELOG.md\n";
    let content = fs::read_to_string("../../CHANGELOG.md").unwrap_or_else(|_| fallback.to_string());
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("changelog.md");
    fs::write(out, content).unwrap();
}
