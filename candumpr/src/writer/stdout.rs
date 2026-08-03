use super::Writer;

/// Writes formatted output to stdout.
pub struct StdoutWriter {
    stdout: std::io::Stdout,
    written: u64,
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
            written: 0,
        }
    }
}

impl std::io::Write for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.stdout.lock().write(buf)?;
        self.written += n as u64;
        Ok(n)
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

    fn bytes_written(&self) -> u64 {
        self.written
    }
}
