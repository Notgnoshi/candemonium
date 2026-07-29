/// Writes formatted frame data to an output destination.
pub trait Writer: std::io::Write {
    /// Flush dirty page cache pages to disk.
    ///
    /// [std::io::Write::flush()] does not necessarily write to disk; it just flushes any
    /// *userspace* buffers owned by the writer into a `write(2)` syscall. This method does an
    /// `fdatasync(2)` to flush any dirty page caches to disk.
    ///
    /// `flush()` can be called cheaply and rapidly. We should not call `sync()` rapidly. It exists
    /// to provide checkpoints where we can be sure that the data has been written to disk,
    /// resulting in the data being recoverable even after power loss.
    fn sync(&mut self) -> std::io::Result<()>;

    /// Finish writing and close the underlying resources.
    ///
    /// Writes any epilogues, flush, and sync. Writes may not be performed after a finish.
    fn finish(&mut self) -> std::io::Result<()>;

    #[cfg(test)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

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

    #[cfg(test)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
