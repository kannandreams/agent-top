//! Incremental line reader for append-only JSONL transcripts.
//!
//! Harnesses append to their transcript on every event, and a busy session can
//! reach tens of megabytes. Re-parsing the whole file each second is not an
//! option, so the reader remembers its byte offset and only returns lines that
//! arrived since the last call. A partial trailing line (a write in progress)
//! is held back until its newline shows up.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct TailReader {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl TailReader {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        TailReader { path: path.into(), offset: 0, partial: Vec::new() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Read up to `budget` new bytes and return the complete lines in them.
    /// Returns `(lines, more_pending)`; `more_pending` is true when the file
    /// still has unread bytes after the budget was spent.
    pub fn read_new_lines(&mut self, budget: usize) -> io::Result<(Vec<String>, bool)> {
        let mut file = File::open(&self.path)?;
        let len = file.metadata()?.len();
        if len < self.offset {
            // Truncated or rotated: start over.
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok((Vec::new(), false));
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let want = (len - self.offset).min(budget as u64) as usize;
        let mut buf = vec![0u8; want];
        let mut read = 0;
        while read < want {
            let n = file.read(&mut buf[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        buf.truncate(read);
        self.offset += read as u64;

        let mut lines = Vec::new();
        let mut start = 0;
        for (i, b) in buf.iter().enumerate() {
            if *b == b'\n' {
                let mut line = std::mem::take(&mut self.partial);
                line.extend_from_slice(&buf[start..i]);
                if !line.is_empty() {
                    lines.push(String::from_utf8_lossy(&line).into_owned());
                }
                start = i + 1;
            }
        }
        self.partial.extend_from_slice(&buf[start..]);
        Ok((lines, self.offset < len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_incrementally_and_holds_partial_lines() {
        let dir = std::env::temp_dir().join(format!("agent-top-jsonl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut f = File::create(&path).unwrap();
        write!(f, "{{\"a\":1}}\n{{\"b\":2").unwrap();
        let mut r = TailReader::new(&path);
        let (lines, more) = r.read_new_lines(1 << 20).unwrap();
        assert_eq!(lines, vec!["{\"a\":1}"]);
        assert!(!more);
        writeln!(f, "}}").unwrap();
        let (lines, _) = r.read_new_lines(1 << 20).unwrap();
        assert_eq!(lines, vec!["{\"b\":2}"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
