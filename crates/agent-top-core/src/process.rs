//! Process enumeration and classification.
//!
//! sysinfo gives us the flat process table; this module decides which
//! processes are agent roots, which are MCP servers, and folds the table into
//! per-agent trees. Everything here is heuristic and documented as such in
//! ADR-002; the harness registry (see `harness::claude`) is preferred when it
//! exists.

use crate::model::{Harness, HostStats, ProcKind, ProcNode};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Debug, Clone)]
pub struct RawProc {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub cmd: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    pub run_time: u64,
}

impl RawProc {
    pub fn cmdline(&self) -> String {
        if self.cmd.is_empty() { self.name.clone() } else { self.cmd.join(" ") }
    }

    /// Basename of argv[0] or the executable, whichever is more informative.
    fn program(&self) -> String {
        let from_cmd = self.cmd.first().map(|c| basename(c));
        let from_exe = self.exe.as_ref().and_then(|e| e.file_name()).map(|f| f.to_string_lossy().into_owned());
        from_cmd.or(from_exe).unwrap_or_else(|| self.name.clone())
    }

    /// The npm launcher forwards argv[2..] unchanged to the native runtime.
    /// Verified against openai/codex rust-v0.154.0, codex-cli/bin/codex.js.
    fn is_codex_launcher(&self) -> bool {
        matches!(self.program().to_ascii_lowercase().as_str(), "node" | "node.exe" | "nodejs" | "bun" | "bun.exe")
            && self.cmd.get(1).is_some_and(|s| s.replace('\\', "/").ends_with("/@openai/codex/bin/codex.js"))
    }

    fn codex_args(&self) -> Option<&[String]> {
        if matches!(self.program().to_ascii_lowercase().as_str(), "codex" | "codex.exe") {
            Some(self.cmd.get(1..).unwrap_or_default())
        } else if self.is_codex_launcher() {
            self.cmd.get(2..)
        } else {
            None
        }
    }

    /// Whether this is the native runtime directly spawned by an npm launcher
    /// for the same invocation, rather than a separately launched agent.
    pub(crate) fn is_codex_runtime_of(&self, parent: &RawProc) -> bool {
        self.ppid == Some(parent.pid)
            && parent.is_codex_launcher()
            && !self.is_codex_launcher()
            && classify_agent(self) == Some(Harness::Codex)
            && self.codex_args() == parent.codex_args()
    }
}

fn basename(s: &str) -> String {
    s.rsplit('/').next().unwrap_or(s).to_string()
}

pub struct ProcessScanner {
    sys: System,
    self_pid: Option<u32>,
}

impl Default for ProcessScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessScanner {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, Self::refresh_kind());
        let self_pid = sysinfo::get_current_pid().ok().map(|p| p.as_u32());
        ProcessScanner { sys, self_pid }
    }

    pub fn refresh(&mut self) {
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, Self::refresh_kind());
    }

    /// What to read per process. `System::refresh_processes` reads memory, CPU
    /// and the executable only; the command line and working directory, which
    /// every classification and attribution heuristic here depends on, have
    /// to be asked for. Each is read once per process (`OnlyIfNotSet`): a
    /// command line never changes, and an agent's working directory does not
    /// change in practice, so the per-tick cost stays at memory and CPU.
    /// `nothing()` still includes Linux tasks by default. Exclude them: worker
    /// threads share the process's command line and RSS, and process CPU already
    /// includes their work. Treating them as children invents subagents and
    /// counts the same resources again for every thread.
    fn refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_memory()
            .with_cpu()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
    }

    pub fn host(&self) -> HostStats {
        HostStats {
            hostname: System::host_name(),
            cpu_percent: self.sys.global_cpu_usage(),
            cpu_count: self.sys.cpus().len(),
            mem_used_bytes: self.sys.used_memory(),
            mem_total_bytes: self.sys.total_memory(),
        }
    }

    pub fn processes(&self) -> Vec<RawProc> {
        self.sys
            .processes()
            .iter()
            .filter(|(pid, _)| Some(pid.as_u32()) != self.self_pid)
            .map(|(pid, p)| RawProc {
                pid: pid.as_u32(),
                ppid: p.parent().map(|x| x.as_u32()),
                name: p.name().to_string_lossy().into_owned(),
                exe: p.exe().map(|e| e.to_path_buf()),
                cmd: p.cmd().iter().map(|c| c.to_string_lossy().into_owned()).collect(),
                cwd: p.cwd().map(|c| c.to_path_buf()),
                cpu_percent: p.cpu_usage(),
                rss_bytes: p.memory(),
                start_time: p.start_time(),
                run_time: p.run_time(),
            })
            .collect()
    }
}

/// Is this process the root of a coding agent? Which harness?
pub fn classify_agent(p: &RawProc) -> Option<Harness> {
    let prog = p.program();
    let prog = prog.strip_suffix(".exe").unwrap_or(&prog).to_ascii_lowercase();
    let joined = p.cmd.join(" ");

    // Node-hosted CLIs show up as `node <path>/cli.js`; look at the script path too.
    let script = p.cmd.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();

    if prog == "claude" || script.contains("@anthropic-ai/claude-code") || script.ends_with("/claude") {
        return Some(Harness::Claude);
    }
    if let Some(args) = p.codex_args() {
        return (!codex_helper(args)).then_some(Harness::Codex);
    }
    if prog == "gemini" || script.contains("@google/gemini-cli") {
        return Some(Harness::Gemini);
    }
    if prog == "opencode" {
        return Some(Harness::OpenCode);
    }
    if prog == "kodelet" {
        let args = kodelet_command(p.cmd.get(1..).unwrap_or_default());
        return (args.first().map(String::as_str) == Some("serve")
            || (args.first().map(String::as_str) == Some("runner") && args.get(1).map(String::as_str) == Some("start")))
        .then_some(Harness::Kodelet);
    }
    if prog == "aider" || joined.contains("aider/main.py") {
        return Some(Harness::Aider);
    }
    if prog == "copilot" || script.contains("@github/copilot") {
        return Some(Harness::Copilot);
    }
    if prog == "cursor-agent" {
        return Some(Harness::Cursor);
    }
    None
}

/// Classify a non-root process by what it looks like.
pub fn classify_child(p: &RawProc) -> ProcKind {
    let prog = p.program().to_ascii_lowercase();
    let joined = p.cmdline().to_ascii_lowercase();
    // In particular, `codex mcp list` manages servers; it is not itself one.
    if p.codex_args().is_some_and(codex_helper) {
        return ProcKind::Tool;
    }
    // Kodelet clients and extension workers are not MCP servers, even if a
    // prompt, extension name or profile happens to contain "mcp".
    if prog == "kodelet" || prog.starts_with("kodelet-extension-") || prog == "kodelet-subagent" {
        return ProcKind::Tool;
    }
    if matches!(prog.as_str(), "zsh" | "bash" | "sh" | "fish" | "dash" | "pwsh" | "cmd") {
        return ProcKind::Shell;
    }
    if looks_like_mcp(&prog, &joined) {
        return ProcKind::Mcp;
    }
    ProcKind::Tool
}

/// Explicit helper invocations from Codex 0.154 and main 7a3c5a83e (2026-09-17)
/// cli/arg0 dispatch. Match positions, never words inside prompts or commands.
/// This is deliberately not a full Codex option parser; unrecognised forms
/// retain the existing harness-name heuristic.
fn codex_helper(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some(
            "--codex-run-as-apply-patch"
                | "--codex-run-as-arg0-exec-helper"
                | "--codex-run-as-fs-helper"
                | "--run-as-windows-sandbox"
                | "--__codex-windows-mxc"
                | "mcp"
                | "sandbox"
                | "exec-server"
                | "stdio-to-uds"
                | "responses-api-proxy"
        )
    ) || (args.first().map(String::as_str) == Some("app-server") && args.get(1).map(String::as_str) == Some("daemon"))
}

/// Kodelet v0.6.17-beta executes conversations in `serve` or `runner start`.
/// `run`, `chat` and `acp` are thin clients, not additional agent hosts.
/// Skip only known persistent flags; never search prompt text for a command.
pub(crate) fn kodelet_command(mut args: &[String]) -> &[String] {
    while let Some(arg) = args.first() {
        let flag = arg.split('=').next().unwrap_or(arg);
        let takes_value = match flag {
            "--provider"
            | "--model"
            | "--max-tokens"
            | "--thinking-budget-tokens"
            | "--weak-model"
            | "--weak-model-max-tokens"
            | "--reasoning-effort"
            | "--log-level"
            | "--log-format"
            | "--allowed-commands"
            | "--allowed-domains-file"
            | "--sysprompt"
            | "--sysprompt-arg"
            | "--allowed-tools"
            | "--tool-mode"
            | "--anthropic-api-access"
            | "--profile"
            | "--context-patterns"
            | "--compact-ratio" => true,
            "--enable-openai-search" | "--no-skills" | "--enable-fs-search-tools" => false,
            _ => break,
        };
        let count = if takes_value && !arg.contains('=') { 2 } else { 1 };
        args = args.get(count..).unwrap_or_default();
    }
    args
}

/// MCP servers have no wire-level marker visible from the process table, so
/// this is purely a naming heuristic. False negatives are expected; ADR-002
/// lists the known ones and RFC-102 proposes a registry-based replacement.
pub fn looks_like_mcp(prog: &str, joined: &str) -> bool {
    prog.contains("mcp")
        || joined.contains("modelcontextprotocol")
        || joined.contains("mcp-server")
        || joined.contains("mcp_server")
        || joined.contains("-mcp ")
        || joined.ends_with("-mcp")
        || joined.contains("mcp-")
        || joined.contains("/mcp/")
        || joined.contains(" mcp ")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fold the flat table into a forest of agent trees plus the orphaned MCP list.
///
/// An agent root is a process that classifies as a harness and has no
/// harness ancestor. Nested harness processes are also `Agent` nodes; their
/// position records OS ancestry, not a logical subagent relationship.
pub fn build_forest(procs: &[RawProc]) -> (Vec<ProcNode>, Vec<ProcNode>) {
    let by_pid: HashMap<u32, &RawProc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.ppid {
            children.entry(pp).or_default().push(p.pid);
        }
    }
    let harness_of: HashMap<u32, Harness> = procs.iter().filter_map(|p| classify_agent(p).map(|h| (p.pid, h))).collect();

    let has_agent_ancestor = |mut pid: u32| -> bool {
        let mut hops = 0;
        while let Some(p) = by_pid.get(&pid) {
            match p.ppid {
                Some(pp) if pp != pid && hops < 64 => {
                    if harness_of.contains_key(&pp) {
                        return true;
                    }
                    pid = pp;
                    hops += 1;
                }
                _ => return false,
            }
        }
        false
    };

    let now = now_secs();
    fn build(
        pid: u32,
        kind: ProcKind,
        by_pid: &HashMap<u32, &RawProc>,
        children: &HashMap<u32, Vec<u32>>,
        harness_of: &HashMap<u32, Harness>,
        now: u64,
        depth: usize,
    ) -> ProcNode {
        let p = by_pid[&pid];
        let mut kids = Vec::new();
        if depth < 32
            && let Some(cs) = children.get(&pid)
        {
            let mut cs = cs.clone();
            cs.sort_unstable();
            for c in cs {
                if c == pid {
                    continue;
                }
                let k = if harness_of.contains_key(&c) { ProcKind::Agent } else { classify_child(by_pid[&c]) };
                kids.push(build(c, k, by_pid, children, harness_of, now, depth + 1));
            }
        }
        ProcNode {
            pid,
            ppid: p.ppid,
            name: p.program(),
            cmdline: p.cmdline(),
            kind,
            harness: harness_of.get(&pid).copied(),
            cpu_percent: p.cpu_percent,
            rss_bytes: p.rss_bytes,
            age_secs: if p.run_time > 0 { p.run_time } else { now.saturating_sub(p.start_time) },
            cwd: p.cwd.clone(),
            children: kids,
        }
    }

    let mut roots: Vec<ProcNode> = harness_of
        .keys()
        .filter(|pid| !has_agent_ancestor(**pid))
        .map(|pid| build(*pid, ProcKind::Agent, &by_pid, &children, &harness_of, now, 0))
        .collect();
    roots.sort_by_key(|r| r.pid);

    // Orphans: MCP-looking processes with no live agent anywhere above them.
    let mut orphans: Vec<ProcNode> = procs
        .iter()
        .filter(|p| !harness_of.contains_key(&p.pid))
        .filter(|p| classify_child(p) == ProcKind::Mcp)
        .filter(|p| !has_agent_ancestor(p.pid))
        .map(|p| ProcNode {
            pid: p.pid,
            ppid: p.ppid,
            name: p.program(),
            cmdline: p.cmdline(),
            kind: ProcKind::Mcp,
            harness: None,
            cpu_percent: p.cpu_percent,
            rss_bytes: p.rss_bytes,
            age_secs: if p.run_time > 0 { p.run_time } else { now.saturating_sub(p.start_time) },
            cwd: p.cwd.clone(),
            children: Vec::new(),
        })
        .collect();
    // Only report the top of each orphaned subtree, not every descendant.
    let orphan_pids: std::collections::HashSet<u32> = orphans.iter().map(|o| o.pid).collect();
    orphans.retain(|o| {
        let mut pid = o.pid;
        let mut hops = 0;
        while let Some(p) = by_pid.get(&pid) {
            match p.ppid {
                Some(pp) if pp != pid && hops < 64 => {
                    if orphan_pids.contains(&pp) {
                        return false;
                    }
                    pid = pp;
                    hops += 1;
                }
                _ => break,
            }
        }
        true
    });
    orphans.sort_by_key(|o| std::cmp::Reverse(o.age_secs));
    (roots, orphans)
}

/// Extract `--resume <id>` / `-r <id>` style session ids from a command line.
pub fn session_id_from_args(cmd: &[String]) -> Option<String> {
    let mut it = cmd.iter();
    while let Some(a) = it.next() {
        if a == "--resume" || a == "-r" || a == "resume" {
            if let Some(v) = it.next()
                && looks_like_uuid(v)
            {
                return Some(v.clone());
            }
        } else if let Some(v) = a.strip_prefix("--resume=")
            && looks_like_uuid(v)
        {
            return Some(v.to_string());
        }
    }
    None
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: Option<u32>, cmd: &[&str]) -> RawProc {
        RawProc {
            pid,
            ppid,
            name: basename(cmd[0]),
            exe: None,
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            cpu_percent: 0.0,
            rss_bytes: 0,
            start_time: 0,
            run_time: 1,
        }
    }

    #[test]
    fn kodelet_execution_hosts_are_agents_but_clients_and_helpers_are_not() {
        for cmd in [
            vec!["kodelet", "serve", "--managed"],
            vec!["/usr/local/bin/kodelet", "serve", "--embedded-runner=false"],
            vec!["kodelet", "runner", "start"],
            vec!["kodelet", "--log-level", "debug", "--profile=coding", "serve"],
            vec!["kodelet", "--no-skills", "runner", "start"],
        ] {
            assert_eq!(classify_agent(&proc(1, None, &cmd)), Some(Harness::Kodelet), "{cmd:?}");
        }
        for cmd in [
            vec!["kodelet"],
            vec!["kodelet", "run", "serve mcp"],
            vec!["kodelet", "chat", "--resume", "20260919T090000-abcdef"],
            vec!["kodelet", "acp"],
            vec!["kodelet", "runner", "list"],
            vec!["kodelet", "server", "logs"],
            vec!["kodelet", "conversation", "show", "abc"],
            vec!["kodelet", "--model", "serve"],
            vec!["kodelet", "--", "serve"],
            vec!["kodelet-extension-code-search"],
            vec!["kodelet-extension-mcp"],
            vec!["kodelet-subagent"],
            vec!["cat", "/usr/local/bin/kodelet"],
        ] {
            let p = proc(1, None, &cmd);
            assert_eq!(classify_agent(&p), None, "{cmd:?}");
            assert_eq!(classify_child(&p), ProcKind::Tool, "{cmd:?}");
        }
    }

    #[test]
    fn refreshes_processes_without_tasks() {
        let kind = ProcessScanner::refresh_kind();
        assert!(!kind.tasks(), "Linux threads share their process's RSS and must not become tree nodes");
        assert!(kind.memory());
        assert!(kind.cpu());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scanner_excludes_live_worker_threads() {
        use std::sync::mpsc;

        std::thread::scope(|scope| {
            let (ready, tid) = mpsc::channel();
            let (_release, wait) = mpsc::channel::<()>();
            scope.spawn(move || {
                let path = std::fs::read_link("/proc/thread-self").unwrap();
                let tid: u32 = path.file_name().unwrap().to_str().unwrap().parse().unwrap();
                ready.send(tid).unwrap();
                // Stay alive during both scans; dropping the sender also releases
                // the worker if an assertion panics.
                let _ = wait.recv();
            });
            let tid = tid.recv().unwrap();
            let pid = std::process::id();
            assert_ne!(tid, pid);

            let mut scanner = ProcessScanner::new();
            // Include the test process so we can check that only its leader is
            // kept, independently of agent-top's normal self-PID exclusion.
            scanner.self_pid = None;
            for _ in 0..2 {
                let procs = scanner.processes();
                assert!(procs.iter().any(|p| p.pid == pid), "the process itself must remain visible");
                assert!(!procs.iter().any(|p| p.pid == tid), "a worker thread is not a child process");
                scanner.refresh();
            }
        });
    }

    #[test]
    fn nested_harnesses_are_agents_with_each_process_counted_once() {
        let mut procs = vec![
            proc(10, None, &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "--yolo"]),
            proc(11, Some(10), &["/opt/codex/bin/codex", "--yolo"]),
            proc(12, Some(11), &["bash", "-c", "codex --yolo"]),
            proc(13, Some(12), &["codex", "--yolo"]),
            proc(14, Some(10), &["codex", "exec", "review"]),
            proc(15, Some(11), &["codex", "--yolo"]),
        ];
        for p in &mut procs {
            p.rss_bytes = 100;
            p.cpu_percent = 1.0;
        }
        let (roots, orphans) = build_forest(&procs);
        assert!(orphans.is_empty());
        assert_eq!(roots.len(), 1);
        let root = &roots[0];
        assert_eq!(root.pid, 10);
        let runtime = &root.children[0];
        assert_eq!(runtime.pid, 11);
        assert_eq!(runtime.kind, ProcKind::Agent);
        assert_eq!(runtime.children[0].kind, ProcKind::Shell);
        assert_eq!(runtime.children[0].children[0].kind, ProcKind::Agent, "tool-launched harnesses are still agents");
        assert_eq!(runtime.children[1].kind, ProcKind::Agent);
        assert_eq!(root.children[1].kind, ProcKind::Agent);
        assert_eq!(root.totals(), (6.0, 600, 6, 0), "each real PID contributes resources exactly once");
    }

    #[test]
    fn codex_runtime_matches_only_its_launcher_and_forwarded_arguments() {
        let launcher = proc(10, None, &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "exec", "review the diff"]);
        let runtime = proc(11, Some(10), &["/opt/codex/bin/codex", "exec", "review the diff"]);
        assert!(runtime.is_codex_runtime_of(&launcher));

        for child in [
            proc(12, Some(10), &["codex", "exec", "different task"]),
            proc(12, Some(10), &["codex", "exec", "review", "the", "diff"]),
            proc(12, Some(99), &["codex", "exec", "review the diff"]),
            proc(12, Some(10), &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "exec", "review the diff"]),
        ] {
            assert!(!child.is_codex_runtime_of(&launcher), "not the forwarded runtime: {child:?}");
        }
        let nested = proc(12, Some(11), &["codex", "exec", "review the diff"]);
        assert!(!nested.is_codex_runtime_of(&runtime), "a native agent is not a launcher");

        let launcher = proc(10, None, &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "mcp", "list"]);
        let helper = proc(11, Some(10), &["codex", "mcp", "list"]);
        assert!(!helper.is_codex_runtime_of(&launcher), "a management helper does not own agent rollouts");
    }

    #[test]
    fn legacy_process_subagent_kind_deserializes_as_agent() {
        let kind: ProcKind = serde_json::from_str("\"subagent\"").unwrap();
        assert_eq!(kind, ProcKind::Agent);
        assert_eq!(kind.label(), "agent");
        assert_eq!(serde_json::to_value(kind).unwrap(), "agent");
    }

    #[test]
    fn codex_helpers_are_tools_not_agents_or_mcp_servers() {
        let helpers: &[&[&str]] = &[
            &["codex", "--codex-run-as-apply-patch", "patch"],
            &["codex", "--codex-run-as-arg0-exec-helper"],
            &["codex", "--codex-run-as-fs-helper"],
            &["codex.exe", "--run-as-windows-sandbox"],
            &["codex.exe", "--__codex-windows-mxc"],
            &["codex", "mcp", "list"],
            &["codex", "sandbox", "--", "sh"],
            &["codex", "exec-server"],
            &["codex", "stdio-to-uds", "/tmp/socket"],
            &["codex", "responses-api-proxy"],
            &["codex", "app-server", "daemon", "start"],
            &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "mcp", "list"],
            &["codex-linux-sandbox", "--", "sh"],
            &["apply_patch", "patch"],
            &["applypatch", "patch"],
        ];
        let mut procs = vec![proc(1, None, &["codex", "--yolo"])];
        for (i, cmd) in helpers.iter().enumerate() {
            let mut p = proc(i as u32 + 2, Some(1), cmd);
            // argv[0] dispatch aliases may all resolve to the Codex executable.
            p.exe = Some(PathBuf::from("/opt/codex/bin/codex"));
            assert_eq!(classify_agent(&p), None, "{cmd:?}");
            assert_eq!(classify_child(&p), ProcKind::Tool, "{cmd:?}");
            procs.push(p);
        }
        let (roots, orphans) = build_forest(&procs);
        assert_eq!(roots.len(), 1);
        assert!(roots[0].children.iter().all(|p| p.kind == ProcKind::Tool));
        assert!(orphans.is_empty());

        for cmd in [
            vec!["codex"],
            vec!["codex", "app-server", "--listen", "stdio://"],
            vec!["codex", "exec", "mcp"],
            vec!["codex", "--", "sandbox"],
            vec!["codex", "review"],
            vec!["codex", "resume", "--last"],
            // This became a subcommand after 0.154, but is a valid prompt in
            // that release. Do not exclude it without knowing the version.
            vec!["codex", "tcp-tunnel"],
        ] {
            assert_eq!(classify_agent(&proc(20, None, &cmd)), Some(Harness::Codex), "{cmd:?}");
        }
        assert_eq!(
            classify_agent(&proc(20, None, &["cat", "/usr/lib/node_modules/@openai/codex/bin/codex.js"])),
            None,
            "mentioning the launcher path is not running it"
        );
    }

    #[test]
    fn classifies_roots_and_children() {
        let procs = vec![
            proc(1, None, &["/sbin/launchd"]),
            proc(10, Some(1), &["claude", "--resume", "a29e19c3-2856-4510-87a0-80ce170ad830"]),
            proc(11, Some(10), &["/bin/zsh", "-c", "cargo test"]),
            proc(12, Some(10), &["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]),
            proc(13, Some(10), &["claude", "-p", "summarise"]),
            proc(20, Some(1), &["uvx", "mcp-server-git"]),
            proc(
                30,
                Some(1),
                &["/Applications/ChatGPT.app/Contents/Frameworks/Codex Framework.framework/Helpers/browser_crashpad_handler"],
            ),
        ];
        let (roots, orphans) = build_forest(&procs);
        assert_eq!(roots.len(), 1);
        let root = &roots[0];
        assert_eq!(root.harness, Some(Harness::Claude));
        let kinds: Vec<ProcKind> = root.children.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec![ProcKind::Shell, ProcKind::Mcp, ProcKind::Agent]);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].pid, 20);
        assert_eq!(session_id_from_args(&procs[1].cmd).as_deref(), Some("a29e19c3-2856-4510-87a0-80ce170ad830"));
    }
}
