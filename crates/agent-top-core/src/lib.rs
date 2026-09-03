//! agent-top-core: discovery, accounting and process-tree model for agent-top.
//!
//! This crate has no terminal dependencies. It answers one question: "which
//! coding agents are on this machine right now, what are they doing, and what
//! have they spent?" The TUI crate renders the answer; `--json` prints it.

pub mod collector;
pub mod harness;
pub mod jsonl;
pub mod model;
pub mod pricing;
pub mod process;

pub use collector::{Collector, CollectorOptions};
pub use model::*;
