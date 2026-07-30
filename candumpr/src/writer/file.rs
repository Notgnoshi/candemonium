use std::io::Write;

use super::Writer;

/// Writes unbuffered formatted output to a file.
///
/// Use [std::io::BufWriter] for buffering, if desired.
pub struct FileWriter {
    file: std::fs::File,
}

impl FileWriter {
    /// Create a new file writer from a pre-opened file
    ///
    /// This lets the caller decide the open options.
    pub fn new(file: std::fs::File) -> Self {
        Self { file }
    }
}

impl std::io::Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
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
}
