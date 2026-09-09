---
description: "The crates, tools and prior art agent-top is built on and inspired by: ratatui, sysinfo, VHS, Material for MkDocs, htop and btop."
title: Credits
hide:
  - toc
---

# Credits

agent-top is one static binary, and most of what makes it work was written by other people. This page names them.

## Built on

<div class="at-cards">

<a class="at-card" href="https://ratatui.rs">
<span class="at-mark">rt</span>
<span class="at-name">ratatui <span class="at-tag">Rendering</span></span>
<p>Draws every box, table, meter, sparkline and popup on the screen.</p>
<span class="at-link">ratatui.rs</span>
</a>

<a class="at-card" href="https://github.com/crossterm-rs/crossterm">
<span class="at-mark">ct</span>
<span class="at-name">crossterm <span class="at-tag">Terminal</span></span>
<p>Raw mode, key events and the alternate screen agent-top draws on.</p>
<span class="at-link">github.com/crossterm-rs/crossterm</span>
</a>

<a class="at-card" href="https://github.com/GuillaumeGomez/sysinfo">
<span class="at-mark">si</span>
<span class="at-name">sysinfo <span class="at-tag">Processes</span></span>
<p>The process table, CPU and memory, on macOS and Linux, without a shell-out.</p>
<span class="at-link">github.com/GuillaumeGomez/sysinfo</span>
</a>

<a class="at-card" href="https://github.com/andrewdavidmackenzie/libproc-rs">
<span class="at-mark">lp</span>
<span class="at-name">libproc <span class="at-tag">macOS</span></span>
<p>Which files a process holds open, which is how a Codex session is matched to its process exactly.</p>
<span class="at-link">github.com/andrewdavidmackenzie/libproc-rs</span>
</a>

<a class="at-card" href="https://github.com/rusqlite/rusqlite">
<span class="at-mark">rq</span>
<span class="at-name">rusqlite <span class="at-tag">SQLite</span></span>
<p>Reads OpenCode's session store read-only, with SQLite compiled into the binary.</p>
<span class="at-link">github.com/rusqlite/rusqlite</span>
</a>

<a class="at-card" href="https://serde.rs">
<span class="at-mark">sd</span>
<span class="at-name">serde <span class="at-tag">Data</span></span>
<p>Parses every transcript and prints the snapshot that <code>--json</code> and <code>--replay</code> share.</p>
<span class="at-link">serde.rs</span>
</a>

<a class="at-card" href="https://github.com/clap-rs/clap">
<span class="at-mark">cl</span>
<span class="at-name">clap <span class="at-tag">Command line</span></span>
<p>Flags, subcommands, help text and the shell completions Homebrew installs.</p>
<span class="at-link">github.com/clap-rs/clap</span>
</a>

<a class="at-card" href="https://github.com/toml-rs/toml">
<span class="at-mark">tm</span>
<span class="at-name">toml <span class="at-tag">Prices</span></span>
<p>Reads the built-in price table and the one you keep in your config directory.</p>
<span class="at-link">github.com/toml-rs/toml</span>
</a>

<a class="at-card" href="https://github.com/algesten/ureq">
<span class="at-mark">uq</span>
<span class="at-name">ureq <span class="at-tag">HTTP</span></span>
<p>The daily version check and the one <code>trace --endpoint</code> post you ask for. No async runtime.</p>
<span class="at-link">github.com/algesten/ureq</span>
</a>

<a class="at-card" href="https://github.com/dtolnay/anyhow">
<span class="at-mark">ah</span>
<span class="at-name">anyhow and thiserror <span class="at-tag">Errors</span></span>
<p>Errors that say what went wrong and where, in the CLI and in the core crate.</p>
<span class="at-link">github.com/dtolnay/anyhow</span>
</a>

</div>

## Uses

<div class="at-cards">

<a class="at-card" href="https://github.com/charmbracelet/vhs">
<span class="at-mark clay">vhs</span>
<span class="at-name">VHS <span class="at-tag">Recording, by Charm</span></span>
<p>Records the demo animation and takes every screenshot of the live view, from a tape file checked into the repo.</p>
<span class="at-link">github.com/charmbracelet/vhs</span>
</a>

<a class="at-card" href="https://github.com/charmbracelet/freeze">
<span class="at-mark clay">fz</span>
<span class="at-name">Freeze <span class="at-tag">Screenshots, by Charm</span></span>
<p>Turns the report and plain-text output into the images on these pages.</p>
<span class="at-link">github.com/charmbracelet/freeze</span>
</a>

<a class="at-card" href="https://squidfunk.github.io/mkdocs-material/">
<span class="at-mark clay">md</span>
<span class="at-name">Material for MkDocs <span class="at-tag">This site</span></span>
<p>Builds these pages from the Markdown in the repository's docs directory.</p>
<span class="at-link">squidfunk.github.io/mkdocs-material</span>
</a>

<a class="at-card" href="https://www.jetbrains.com/lp/mono/">
<span class="at-mark clay">jb</span>
<span class="at-name">JetBrains Mono <span class="at-tag">Type</span></span>
<p>The face of everything you read on this site, text and code alike, served from the site itself under the SIL Open Font License.</p>
<span class="at-link">jetbrains.com/lp/mono</span>
</a>

<a class="at-card" href="https://vercel.com/font">
<span class="at-mark clay">gt</span>
<span class="at-name">Geist <span class="at-tag">Type, by Vercel</span></span>
<p>The face of every heading and label on this site, served from the site itself under the SIL Open Font License.</p>
<span class="at-link">vercel.com/font</span>
</a>

<a class="at-card" href="https://mermaid.js.org">
<span class="at-mark clay">mm</span>
<span class="at-name">Mermaid <span class="at-tag">Diagrams</span></span>
<p>Draws the data-flow diagram on the architecture page, rendered to SVG at writing time so the page loads no script.</p>
<span class="at-link">mermaid.js.org</span>
</a>

<a class="at-card" href="https://pages.cloudflare.com">
<span class="at-mark clay">cf</span>
<span class="at-name">Cloudflare Pages <span class="at-tag">Hosting</span></span>
<p>Serves the built site, rebuilt from the main branch on every push.</p>
<span class="at-link">pages.cloudflare.com</span>
</a>

<a class="at-card" href="https://ui.perfetto.dev">
<span class="at-mark clay">pf</span>
<span class="at-name">Perfetto <span class="at-tag">Trace viewer</span></span>
<p>Opens the Chrome trace files that <code>agent-top trace</code> writes.</p>
<span class="at-link">ui.perfetto.dev</span>
</a>

<a class="at-card" href="https://opentelemetry.io">
<span class="at-mark clay">ot</span>
<span class="at-name">OpenTelemetry <span class="at-tag">Trace format</span></span>
<p>The OTLP format the trace export speaks, so Jaeger, Tempo and any collector can read a session.</p>
<span class="at-link">opentelemetry.io</span>
</a>

</div>

## In the spirit of

<div class="at-cards">

<a class="at-card" href="https://htop.dev">
<span class="at-mark straw">ht</span>
<span class="at-name">htop <span class="at-tag">The idea</span></span>
<p>One screen, every process, refreshed every second. agent-top is that screen for coding agents.</p>
<span class="at-link">htop.dev</span>
</a>

<a class="at-card" href="https://github.com/aristocratos/btop">
<span class="at-mark straw">bt</span>
<span class="at-name">btop <span class="at-tag">The look</span></span>
<p>Meters, ramps and a whole machine on one screen. The header and the colour ramps owe it a debt.</p>
<span class="at-link">github.com/aristocratos/btop</span>
</a>

</div>

## The harnesses it reads

<div class="at-cards">

<a class="at-card" href="https://claude.com/claude-code">
<span class="at-mark">cc</span>
<span class="at-name">Claude Code <span class="at-tag">Anthropic</span></span>
<p>Per-pid session registry and JSONL transcripts. The exact attribution path.</p>
<span class="at-link">claude.com/claude-code</span>
</a>

<a class="at-card" href="https://github.com/openai/codex">
<span class="at-mark">cx</span>
<span class="at-name">Codex <span class="at-tag">OpenAI</span></span>
<p>Rollout files with usage, rate limits and MCP call events on every record.</p>
<span class="at-link">github.com/openai/codex</span>
</a>

<a class="at-card" href="https://github.com/google-gemini/gemini-cli">
<span class="at-mark">gm</span>
<span class="at-name">Gemini CLI <span class="at-tag">Google</span></span>
<p>Session chats with usage per turn and MCP tools named by server.</p>
<span class="at-link">github.com/google-gemini/gemini-cli</span>
</a>

<a class="at-card" href="https://opencode.ai">
<span class="at-mark">oc</span>
<span class="at-name">OpenCode <span class="at-tag">Open source</span></span>
<p>A SQLite session store with its own cost figure, read in place and read-only.</p>
<span class="at-link">opencode.ai</span>
</a>

</div>

agent-top is MIT licensed. The crate dependency list, with every version, is in [`Cargo.lock`](https://github.com/kannandreams/agent-top/blob/main/Cargo.lock).
