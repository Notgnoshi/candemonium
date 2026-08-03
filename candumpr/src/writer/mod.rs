mod file;
mod stdout;
mod zstd;

use std::io::Write;

pub use file::FileWriter;
pub use stdout::StdoutWriter;
pub use zstd::ZstdWriter;

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

    /// Bytes this writer has written
    ///
    /// This counts the number of bytes that were written to disk, not the number of bytes that were
    /// passed into the writer to write. [ZstdWriter] is an example of a writer where bytes in !=
    /// bytes out.
    fn bytes_written(&self) -> u64;
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

    fn bytes_written(&self) -> u64 {
        self.get_ref().bytes_written()
    }
}
