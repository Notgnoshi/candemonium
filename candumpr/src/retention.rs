use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
}
