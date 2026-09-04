//! Which files a process has open.
//!
//! Used to attribute a Codex rollout to the process writing it: the
//! app-server keeps every live thread's rollout open and closes it when the
//! thread ends, which is the one signal that ties a thread to a process
//! without guessing. Only the calling user's processes are visible, which is
//! all agent-top looks at.
//!
//! `None` means the question could not be answered: a platform without
//! support, or a process that is gone or belongs to another user. The caller
//! falls back to its heuristics then, and only then; an empty `Some` is a
//! definite "this process holds no file".

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
    let rd = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
    Some(rd.flatten().filter_map(|e| std::fs::read_link(e.path()).ok()).filter(|p| p.is_absolute()).collect())
}

#[cfg(target_os = "macos")]
pub fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::file_info::{ListFDs, PIDFDInfo, PIDFDInfoFlavor, ProcFDType, pidfdinfo};
    use libproc::libproc::proc_pid::{listpidinfo, pidinfo};

    /// Darwin's `vnode_fdinfowithpath`: a `proc_fileinfo` (24 bytes) and a
    /// `vnode_info` (152 bytes), then the path, `MAXPATHLEN` bytes and
    /// NUL-terminated. The leading fields are not needed and are kept as
    /// opaque bytes; only the total size and the offset of the path matter,
    /// and the test below checks both against the kernel.
    #[repr(C)]
    struct VnodePathInfo {
        _head: [u8; 176],
        path: [u8; 1024],
    }
    impl Default for VnodePathInfo {
        fn default() -> Self {
            VnodePathInfo { _head: [0; 176], path: [0; 1024] }
        }
    }
    impl PIDFDInfo for VnodePathInfo {
        fn flavor() -> PIDFDInfoFlavor {
            PIDFDInfoFlavor::VNodePathInfo
        }
    }

    let pid = pid as i32;
    let info = pidinfo::<BSDInfo>(pid, 0).ok()?;
    let fds = listpidinfo::<ListFDs>(pid, info.pbi_nfiles as usize).ok()?;
    let paths = fds
        .iter()
        .filter(|fd| fd.proc_fdtype == ProcFDType::VNode as u32)
        .filter_map(|fd| pidfdinfo::<VnodePathInfo>(pid, fd.proc_fd).ok())
        .filter_map(|v| {
            let end = v.path.iter().position(|&b| b == 0).unwrap_or(v.path.len());
            let s = std::str::from_utf8(&v.path[..end]).ok()?;
            (!s.is_empty()).then(|| PathBuf::from(s))
        })
        .collect();
    Some(paths)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn open_files(_pid: u32) -> Option<Vec<PathBuf>> {
    None
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    /// Open a file in this process and find it in this process's list. On
    /// macOS this also proves the hand-written struct layout: a wrong size
    /// or path offset gives garbage, not this path.
    #[test]
    fn lists_a_file_this_process_holds_open() {
        let path = std::env::temp_dir().join(format!("agent-top-openfiles-{}.jsonl", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let open = open_files(std::process::id()).expect("own process is readable");
        assert!(open.contains(&canonical), "{canonical:?} not in {open:?}");
        drop(file);
        std::fs::remove_file(&path).unwrap();
        let open = open_files(std::process::id()).unwrap();
        assert!(!open.contains(&canonical), "closed files are not listed");
    }

    #[test]
    fn a_missing_process_cannot_be_answered() {
        assert_eq!(open_files(u32::MAX - 1), None);
    }
}
