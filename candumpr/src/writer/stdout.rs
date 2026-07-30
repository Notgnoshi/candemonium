use super::Writer;

/// Writes formatted output to stdout.
pub struct StdoutWriter {
    stdout: std::io::Stdout,
}

impl Default for StdoutWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl StdoutWriter {
    pub fn new() -> Self {
        Self {
            stdout: std::io::stdout(),
        }
    }
}

impl std::io::Write for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stdout.lock().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stdout.flush()
    }
}

impl Writer for StdoutWriter {
    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
