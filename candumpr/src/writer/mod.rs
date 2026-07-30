mod file;
mod stdout;

use std::io::Write;

pub use file::FileWriter;
pub use stdout::StdoutWriter;

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
}

impl<W: Writer + 'static> Writer for std::io::BufWriter<W> {
    fn sync(&mut self) -> std::io::Result<()> {
        self.flush()?;
        self.get_mut().sync()
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.flush()?;
        self.get_mut().finish()
    }
}
