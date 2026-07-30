use std::process::{Child, Output, Stdio};

pub use assert_cmd::Command;

pub trait CommandExt {
    /// Same as [std::process::Command::output] except with hooks to print stdout/stderr in failed
    /// tests
    fn captured_output(&mut self) -> std::io::Result<Output>;
    /// Spawn with both output streams piped, as every test that signals the child needs.
    fn spawn_piped(&mut self) -> std::io::Result<Child>;
}

impl CommandExt for std::process::Command {
    fn captured_output(&mut self) -> std::io::Result<Output> {
        let output = self.output()?;

        // libtest injects magic in print! macros to capture output in tests
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));

        Ok(output)
    }

    fn spawn_piped(&mut self) -> std::io::Result<Child> {
        self.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
    }
}

pub trait ChildExt {
    /// Send `signal` to the child. [libc::SIGINT], [libc::SIGTERM], etc.
    fn signal(&self, signal: libc::c_int) -> std::io::Result<()>;
    /// Wait for the child to exit, printing stdout/stderr for test visibility.
    ///
    /// Drains both pipes while waiting, so a chatty child cannot wedge on a full pipe.
    fn captured_output(self) -> std::io::Result<Output>;
}

impl ChildExt for Child {
    fn signal(&self, signal: libc::c_int) -> std::io::Result<()> {
        if unsafe { libc::kill(self.id() as libc::pid_t, signal) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn captured_output(self) -> std::io::Result<Output> {
        // wait_with_output reads both pipes before reaping, so a child that fills one cannot
        // deadlock us. A try_wait polling loop does not drain, and would.
        let output = self.wait_with_output()?;

        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));

        Ok(output)
    }
}

/// Command to run a binary target of the calling crate, at the given log level (default TRACE).
///
/// `env!` expands at the call site, and cargo sets `CARGO_BIN_EXE_<name>` only for integration test
/// and bench targets of the package that defines the binary, so this works only there.
///
/// # Example
/// ```ignore
/// use vcan_fixture::prelude::*;
///
/// let output = tool!("candumpr", "INFO")
///     .arg("--help")
///     .captured_output()
///     .unwrap();
/// ```
#[macro_export]
macro_rules! tool {
    ($name:literal) => {
        $crate::tool!($name, "TRACE")
    };
    ($name:literal, $level:literal) => {{
        let mut cmd = ::std::process::Command::new(env!(concat!("CARGO_BIN_EXE_", $name)));
        cmd.arg(concat!("--log-level=", $level));
        cmd
    }};
}
