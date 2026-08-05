use std::io::Write;
use std::path::{Path, PathBuf};

use super::Writer;

/// Writes unbuffered formatted output to a file.
///
/// Use [std::io::BufWriter] for buffering, if desired.
pub struct FileWriter {
    file: std::fs::File,
    path: PathBuf,
    written: u64,
}

impl FileWriter {
    /// Create a new file writer from a pre-opened file
    ///
    /// This lets the caller decide the open options. `path` is where the caller opened the file;
    /// it is only reported back through [Writer::path], never reopened.
    pub fn new(file: std::fs::File, path: PathBuf) -> Self {
        Self {
            file,
            path,
            written: 0,
        }
    }
}

impl std::io::Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Writer for FileWriter {
    fn sync(&mut self) -> std::io::Result<()> {
        self.file.sync_data()
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.flush()?;
        self.sync()
    }

    fn bytes_written(&self) -> u64 {
        self.written
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }
}
