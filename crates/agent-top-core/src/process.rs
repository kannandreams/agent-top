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
    fn refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
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
    if prog == "codex" || script.contains("@openai/codex") {
        return Some(Harness::Codex);
    }
    if prog == "gemini" || script.contains("@google/gemini-cli") {
        return Some(Harness::Gemini);
    }
    if prog == "opencode" {
        return Some(Harness::OpenCode);
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
    if matches!(prog.as_str(), "zsh" | "bash" | "sh" | "fish" | "dash" | "pwsh" | "cmd") {
        return ProcKind::Shell;
    }
    if looks_like_mcp(&prog, &joined) {
        return ProcKind::Mcp;
    }
    ProcKind::Tool
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
/// harness ancestor. Harness processes nested under a root become
/// `Subagent` nodes of that root's tree.
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
                let k = if harness_of.contains_key(&c) { ProcKind::Subagent } else { classify_child(by_pid[&c]) };
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
        assert_eq!(kinds, vec![ProcKind::Shell, ProcKind::Mcp, ProcKind::Subagent]);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].pid, 20);
        assert_eq!(session_id_from_args(&procs[1].cmd).as_deref(), Some("a29e19c3-2856-4510-87a0-80ce170ad830"));
    }
}
