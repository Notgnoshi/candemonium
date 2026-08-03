use std::path::PathBuf;

use clap::Parser;

use crate::format::TimestampMode;

/// The first interface name that appears more than once.
pub fn first_duplicate(interfaces: &[String]) -> Option<&str> {
    let mut seen = std::collections::HashSet::new();
    interfaces
        .iter()
        .find(|name| !seen.insert(name.as_str()))
        .map(String::as_str)
}

/// Output format for received frames.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Format {
    /// can-utils candump file format: `(ts) iface ID#DATA`.
    CandumpFile,
    /// can-utils candump console format: `(ts) iface ID [len] B0 B1 ...`.
    CandumpConsole,
}

impl Format {
    /// Log file extension for each output format
    pub fn ext(&self, compress: bool) -> &'static str {
        match (self, compress) {
            (Format::CandumpFile, false) => "log",
            (Format::CandumpFile, true) => "log.zst",
            (Format::CandumpConsole, false) => "txt",
            (Format::CandumpConsole, true) => "txt.zst",
        }
    }
}

/// Log CAN traffic from multiple networks.
#[derive(Parser)]
#[command(version)]
pub struct Cli {
    /// CAN interfaces to listen on.
    #[arg(required = true)]
    pub interfaces: Vec<String>,

    /// Log each interface to its own file in the current directory, instead of stdout.
    #[arg(long, short = 'l', conflicts_with = "output")]
    pub log: bool,

    /// Log to this file path. Truncated if it already exists.
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Output format for received frames.
    #[arg(long, value_enum, default_value = "candump-file")]
    pub format: Format,

    /// Compress output with zstd. Requires --log or --output.
    #[arg(long, short = 'c')]
    pub compress: bool,

    /// Timestamp rendering mode. Only applies to the candump formats.
    #[arg(long, value_enum, default_value = "absolute")]
    pub timestamp: TimestampMode,

    /// Log level for tracing output on stderr.
    #[arg(long, default_value = "INFO")]
    pub log_level: tracing::Level,
}
