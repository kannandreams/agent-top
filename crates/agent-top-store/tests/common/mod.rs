//! Helpers shared by the store's integration tests.
#![allow(dead_code)]

use agent_top_core::Harness;
use agent_top_store::Source;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-top-core/tests/fixtures").join(name)
}

/// A fresh directory under the system temp dir, removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!("agent-top-store-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A copy of a fixture in `dir`, so a test can append to it or delete it.
pub fn copy(dir: &TempDir, name: &str) -> PathBuf {
    let to = dir.0.join(name);
    std::fs::copy(fixture(name), &to).unwrap();
    to
}

pub fn source(harness: Harness, path: &Path) -> Source {
    Source { harness, id: path.file_stem().unwrap().to_string_lossy().into_owned(), path: path.to_path_buf() }
}
