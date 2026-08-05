use std::path::{Path, PathBuf};
use std::time::SystemTime;

use eyre::WrapErr;

use crate::config::RetentionLimit;

/// Enforces a [RetentionLimit] for one interface's log directory.
///
/// The Sink feeds it lifecycle events: [activated](Self::activated) when a file becomes the
/// active one, and [wrote](Self::wrote) as bytes land in it.
///
/// The file currently being written is never deleted.
pub struct RetentionPolicy {
    limit: RetentionLimit,
    dir: PathBuf,
    /// The file currently being written.
    current: Option<PathBuf>,
    /// On-disk bytes of everything except the current file, as of the last scan.
    base_bytes: u64,
}

impl RetentionPolicy {
    pub fn new(limit: RetentionLimit, dir: PathBuf) -> Self {
        RetentionPolicy {
            limit,
            dir,
            current: None,
            base_bytes: 0,
        }
    }

    /// A new file became the active one
    pub fn activated(&mut self, current: &Path) -> eyre::Result<()> {
        if self.limit == RetentionLimit::Off {
            return Ok(());
        }
        self.current = Some(current.to_path_buf());
        self.enforce()
    }

    /// The current file has grown to `bytes_written` bytes on disk.
    ///
    /// Only [RetentionLimit::Size] reacts to writes; the other modes wait for the next activation.
    pub fn wrote(&mut self, bytes_written: u64) -> eyre::Result<()> {
        let RetentionLimit::Size(limit) = self.limit else {
            return Ok(());
        };
        if self.base_bytes + bytes_written <= limit {
            return Ok(());
        }
        self.enforce()
    }

    /// Rescan the directory, delete enough to satisfy the limit, and recompute the base size.
    fn enforce(&mut self) -> eyre::Result<()> {
        let entries = deletion_candidates(&self.dir)
            .wrap_err_with(|| format!("failed to scan {}", self.dir.display()))?;

        let total: u64 = entries.iter().map(|entry| entry.len).sum();
        let current_len = entries
            .iter()
            .find(|entry| self.current.as_deref() == Some(entry.path.as_path()))
            .map_or(0, |entry| entry.len);
        self.base_bytes = total - current_len;
        for entry in self.doomed(&entries) {
            remove(&entry.path)?;
            self.base_bytes -= entry.len;
        }
        Ok(())
    }

    /// Select which of `entries` must go to satisfy the limit, preserving their order.
    fn doomed<'a>(&self, entries: &'a [Entry]) -> Vec<&'a Entry> {
        // Never delete the file being written.
        let candidates = entries
            .iter()
            .filter(|entry| self.current.as_deref() != Some(entry.path.as_path()));
        match self.limit {
            RetentionLimit::Off => Vec::new(),
            RetentionLimit::Size(limit) => {
                let mut total: u64 = entries.iter().map(|entry| entry.len).sum();
                let mut doomed = Vec::new();
                for entry in candidates {
                    // TODO: Do we want to only delete what it takes to stay under the limit, or
                    // should we free up a little bit of extra overhead too?
                    if total <= limit {
                        break;
                    }
                    doomed.push(entry);
                    total -= entry.len;
                }
                doomed
            }
            RetentionLimit::Files(limit) => {
                let excess = (entries.len() as u64).saturating_sub(limit) as usize;
                candidates.take(excess).collect()
            }
            RetentionLimit::Age(age) => match SystemTime::now().checked_sub(age) {
                None => Vec::new(),
                Some(cutoff) => candidates.filter(|entry| entry.mtime < cutoff).collect(),
            },
        }
    }
}

fn remove(path: &Path) -> eyre::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "deleted by retention policy");
        }
        // Someone else freed it between the scan and here; that was the goal anyway.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).wrap_err_with(|| format!("failed to delete {}", path.display())),
    }
    Ok(())
}

/// One plain file in an interface's log directory.
#[derive(Debug)]
struct Entry {
    path: PathBuf,
    /// Apparent length: what the file contributes to the directory's total size.
    len: u64,
    mtime: SystemTime,
    /// The filename index, when this is one of our own log files.
    index: Option<u64>,
}

/// Parse the index of a log file starting with `i<digits>_`.
fn parse_index(name: &str) -> Option<u64> {
    let (index, _rest) = name.strip_prefix('i')?.split_once('_')?;
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    index.parse().ok()
}

/// Scan `dir` and return its plain files in deletion order: foreign files first (oldest mtime
/// first), then our own log files (ascending index).
fn deletion_candidates(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut foreign = Vec::new();
    let mut own = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let (file_type, metadata) = match entry.file_type().and_then(|t| Ok((t, entry.metadata()?)))
        {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !file_type.is_file() {
            continue;
        }
        let index = entry.file_name().to_str().and_then(parse_index);
        let entry = Entry {
            path: entry.path(),
            len: metadata.len(),
            mtime: metadata.modified()?,
            index,
        };
        match index {
            Some(_) => own.push(entry),
            None => foreign.push(entry),
        }
    }
    foreign.sort_by_key(|entry| entry.mtime);
    own.sort_by_key(|entry| entry.index);
    foreign.append(&mut own);
    Ok(foreign)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;

    /// Create `name` in `dir` with `len` apparent bytes and an mtime `age` in the past.
    fn plant(dir: &TempDir, name: &str, len: u64, age: Duration) {
        let file = std::fs::File::create(dir.path().join(name)).unwrap();
        file.set_len(len).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn scan_orders_foreign_by_age_before_own_by_index() {
        let dir = TempDir::new().unwrap();
        let hour = Duration::from_secs(3600);

        // Own files, planted out of index order; age must not matter for them.
        plant(&dir, "i0002_can0_2024-11-20T15-43-05Z.log", 30, 9 * hour);
        plant(&dir, "i0000_can0_2024-11-20T15-43-05Z.log", 10, hour);
        plant(&dir, "i0010_someone_renamed_this.bak", 50, 2 * hour);

        // Foreign files, ordered among themselves by mtime, oldest first.
        plant(&dir, "notes.txt", 7, 3 * hour);
        plant(&dir, "core.1234", 9, 2 * hour);
        plant(&dir, "i+2_ndex.html", 11, hour);

        // directories and symlinks are never candidates for deletion
        std::fs::create_dir(dir.path().join("i0001_a_subdirectory")).unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("notes.txt"),
            dir.path().join("a_symlink.txt"),
        )
        .unwrap();

        let entries = deletion_candidates(dir.path()).unwrap();
        let seen: Vec<(&str, Option<u64>, u64)> = entries
            .iter()
            .map(|entry| {
                (
                    entry.path.file_name().unwrap().to_str().unwrap(),
                    entry.index,
                    entry.len,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("notes.txt", None, 7),
                ("core.1234", None, 9),
                ("i+2_ndex.html", None, 11),
                ("i0000_can0_2024-11-20T15-43-05Z.log", Some(0), 10),
                ("i0002_can0_2024-11-20T15-43-05Z.log", Some(2), 30),
                ("i0010_someone_renamed_this.bak", Some(10), 50),
            ]
        );
    }

    /// Grow (or shrink) an already-planted file to `len` apparent bytes.
    fn resize(dir: &TempDir, name: &str, len: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(dir.path().join(name))
            .unwrap();
        file.set_len(len).unwrap();
    }

    /// The sorted file names left in `dir`.
    fn names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn size_limit_deletes_just_enough_foreign_first_and_protects_current() {
        let dir = TempDir::new().unwrap();
        let hour = Duration::from_secs(3600);
        plant(&dir, "junk.bin", 60, hour);
        plant(&dir, "i0000_can0.log", 100, 4 * hour);
        plant(&dir, "i0001_can0.log", 90, 3 * hour);
        plant(&dir, "i0002_can0.log", 10, 2 * hour);

        let mut policy = RetentionPolicy::new(RetentionLimit::Size(300), dir.path().to_path_buf());
        // 260 bytes total: activation scans but is under the limit, nothing goes.
        policy
            .activated(&dir.path().join("i0002_can0.log"))
            .unwrap();
        assert_eq!(
            names(&dir),
            [
                "i0000_can0.log",
                "i0001_can0.log",
                "i0002_can0.log",
                "junk.bin"
            ]
        );

        // 310 bytes total: junk.bin alone brings it back under, so the own files survive, even
        //     though every own file is older than the foreign one.
        resize(&dir, "i0002_can0.log", 60);
        policy.wrote(60).unwrap();
        assert_eq!(
            names(&dir),
            ["i0000_can0.log", "i0001_can0.log", "i0002_can0.log"]
        );

        // 540 bytes total: everything deletable goes, and the current file survives at 350
        // bytes even though it exceeds the whole limit by itself.
        resize(&dir, "i0002_can0.log", 350);
        policy.wrote(350).unwrap();
        assert_eq!(names(&dir), ["i0002_can0.log"]);
    }

    #[test]
    fn age_limit_respects_the_cutoff_and_protections() {
        let dir = TempDir::new().unwrap();
        let hour = Duration::from_secs(3600);
        plant(&dir, "old.txt", 10, 3 * hour);
        plant(&dir, "young.txt", 10, Duration::from_secs(60));
        plant(&dir, "i0000_can0.log", 10, 4 * hour);
        plant(&dir, "i0001_can0.log", 10, 3 * hour);
        // The current file is old enough to delete, but protected.
        plant(&dir, "i0002_can0.log", 10, 3 * hour);

        let mut policy = RetentionPolicy::new(RetentionLimit::Age(hour), dir.path().to_path_buf());
        policy
            .activated(&dir.path().join("i0002_can0.log"))
            .unwrap();
        assert_eq!(names(&dir), ["i0002_can0.log", "young.txt"]);
    }

    #[test]
    fn file_count_limit_counts_own_and_foreign_alike() {
        let dir = TempDir::new().unwrap();
        let hour = Duration::from_secs(3600);
        plant(&dir, "a.txt", 10, 2 * hour);
        plant(&dir, "b.txt", 10, hour);
        plant(&dir, "i0000_can0.log", 10, 3 * hour);
        plant(&dir, "i0001_can0.log", 10, 2 * hour);
        plant(&dir, "i0002_can0.log", 10, hour);

        let mut policy = RetentionPolicy::new(RetentionLimit::Files(2), dir.path().to_path_buf());
        policy
            .activated(&dir.path().join("i0002_can0.log"))
            .unwrap();
        assert_eq!(names(&dir), ["i0001_can0.log", "i0002_can0.log"]);
    }
}
