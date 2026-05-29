use std::io::Write;

/// Writes formatted frame data to an output destination.
///
/// Each leaf decides explicitly what each method does. There are no defaults; a leaf with nothing
/// to do for a given hook returns Ok(()).
pub trait Writer {
    fn write(&mut self, buf: &[u8]) -> eyre::Result<()>;
    fn flush(&mut self) -> eyre::Result<()>;
    fn sync(&mut self) -> eyre::Result<()>;
    fn finish(&mut self) -> eyre::Result<()>;

    #[cfg(test)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Writes formatted output to stdout.
///
/// Each [Self::write] acquires the stdout lock, writes the full buffer, and flushes;
/// flush/sync/finish are no-ops.
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

impl Writer for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> eyre::Result<()> {
        let mut lock = self.stdout.lock();
        lock.write_all(buf)?;
        lock.flush()?;
        Ok(())
    }

    fn flush(&mut self) -> eyre::Result<()> {
        Ok(())
    }

    fn sync(&mut self) -> eyre::Result<()> {
        Ok(())
    }

    fn finish(&mut self) -> eyre::Result<()> {
        Ok(())
    }

    #[cfg(test)]
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
