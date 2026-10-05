# agent-top-store

The SQLite history store behind `agent-top sync`, part of
[`agent-top`](https://crates.io/crates/agent-top).

**If you want the tool, install `agent-top` instead.** This crate is published
because `agent-top` depends on it, and so that other tools can fill or read the
same file.

Harnesses delete their own transcripts (Claude Code after `cleanupPeriodDays`,
30 by default). The store keeps what agent-top computed from them: one row per
session, span and MCP server, metadata only.

```rust
use agent_top_store::{Store, default_path};

let mut store = Store::open(&default_path().unwrap())?;
let stats = store.sync(None)?;
println!("{} stored, {} unchanged", stats.stored, stats.unchanged);
# Ok::<(), anyhow::Error>(())
```

The schema is documented at <https://agenttop.dev/history/>.

Licensed under MIT. Source, roadmap and the tool itself are at
<https://github.com/kannandreams/agent-top>.
