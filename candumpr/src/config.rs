use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use eyre::WrapErr;

use crate::format::TimestampMode;
use crate::quantity::Quantity;
use crate::sink::{DEFAULT_FLUSH_INTERVAL, DEFAULT_SYNC_INTERVAL, Output};

/// The first interface name that appears more than once.
pub fn first_duplicate(interfaces: &[String]) -> Option<&str> {
    let mut seen = std::collections::HashSet::new();
    interfaces
        .iter()
        .find(|name| !seen.insert(name.as_str()))
        .map(String::as_str)
}

/// Output format for received frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    /// CAN interfaces to listen on.
    #[arg(required_unless_present = "daemon", conflicts_with = "daemon")]
    pub interfaces: Vec<String>,

    /// Run in daemon mode, using the given config file. Incompatible with all other CLI arguments.
    #[arg(long, value_name = "FILE")]
    pub daemon: Option<PathBuf>,

    /// Log each interface to its own file in the current directory, instead of stdout.
    #[arg(long, short = 'l', conflicts_with_all = ["output", "daemon"])]
    pub log: bool,

    /// Log to this file path. Truncated if it already exists.
    #[arg(long, short = 'o', value_name = "FILE", conflicts_with = "daemon")]
    pub output: Option<PathBuf>,

    /// Output format for received frames.
    #[arg(
        long,
        value_enum,
        default_value = "candump-file",
        conflicts_with = "daemon"
    )]
    pub format: Format,

    /// Compress output with zstd. Requires --log or --output.
    #[arg(long, short = 'c', conflicts_with = "daemon")]
    pub compress: bool,

    /// Timestamp rendering mode. Only applies to the candump formats.
    #[arg(
        long,
        value_enum,
        default_value = "absolute",
        conflicts_with = "daemon"
    )]
    pub timestamp: TimestampMode,

    /// Rotate the log when it exceeds a size ("100MB") or an age ("30min"). "off" disables.
    #[arg(long, value_name = "LIMIT", requires = "log", default_value = "30 minutes", conflicts_with_all = ["output", "daemon"])]
    pub rotate_every: Rotation,

    /// Log level for tracing output on stderr.
    #[arg(long, default_value = "INFO")]
    pub log_level: tracing::Level,
}

/// Logging configuration parsed from CLI or a TOML config file
#[derive(Debug)]
pub struct Config {
    /// Interfaces to log
    ///
    /// NOTE: The order of interfaces in this vector needs to be stable. It's used in internal
    /// bookkeeping to associate sockets with sinks.
    pub interfaces: Vec<String>,
    /// Either exactly one interleaved stream, or one stream for each interface.
    pub streams: Vec<StreamConfig>,
    /// Whether recoverable [Sink](crate::sink::Sink) activation failures are retried or are fatal.
    pub retry_activation_failures: bool,
}

/// Configuration for one output stream
#[derive(Debug, PartialEq, Eq)]
pub struct StreamConfig {
    pub output: Output,
    pub format: Format,
    pub timestamp: TimestampMode,
    pub compress: bool,
    pub flush_interval: Option<Duration>,
    pub sync_interval: Option<Duration>,
    pub rotation: Rotation,
}

/// A log rotation trigger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rotation {
    #[default]
    Off,
    /// Number of bytes written to disk
    Size(u64),
    /// Time since file was opened
    Interval(Duration),
}

impl std::str::FromStr for Rotation {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        match raw.parse::<Quantity>()? {
            Quantity::Off => Ok(Rotation::Off),
            // A small size rotation trigger is very likely a typo.
            Quantity::Bytes(size) if size < 100 => {
                Err(format!("size must be bigger than 100B, got: {raw:?}"))
            }
            Quantity::Bytes(size) => Ok(Rotation::Size(size)),
            Quantity::Duration(duration) if duration.is_zero() => {
                Err(format!("rotation duration must not be zero, got: {raw:?}"))
            }
            Quantity::Duration(duration) => Ok(Rotation::Interval(duration)),
            Quantity::Count(_) => Err("rotation trigger cannot be a file count".into()),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Rotation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Deserialize a flush or sync interval: a duration, or `"off"`.
fn deserialize_interval<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error> {
    use serde::Deserialize;
    let raw = String::deserialize(deserializer)?;
    match raw.parse::<Quantity>().map_err(serde::de::Error::custom)? {
        Quantity::Off => Ok(None),
        Quantity::Duration(duration) => Ok(Some(duration)),
        Quantity::Bytes(_) => Err(serde::de::Error::custom(format!(
            "size-based intervals are not yet supported, got {raw:?}"
        ))),
        Quantity::Count(_) => Err(serde::de::Error::custom(format!(
            "an interval cannot be a file count, got {raw:?}"
        ))),
    }
}

/// Per-interface TOML settings.
///
/// This is the `[defaults]` and each `[interface.<name>]` table.
//
// NOTE: As more fields are added, please keep the `docs/user/candumpr-configuration.md` document
// up-to-date.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
struct RawStreamConfig {
    format: Format,
    compress: bool,
    timestamp: TimestampMode,
    // This is the one TOML setting that's required; the rest have default values taken from
    // [RawStreamConfig::default].
    directory: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_interval")]
    flush_every: Option<Duration>,
    #[serde(deserialize_with = "deserialize_interval")]
    sync_every: Option<Duration>,
    rotate_every: Rotation,
}

impl Default for RawStreamConfig {
    fn default() -> Self {
        RawStreamConfig {
            format: Format::CandumpFile,
            compress: true,
            timestamp: TimestampMode::Absolute,
            directory: None,
            flush_every: Some(DEFAULT_FLUSH_INTERVAL),
            sync_every: Some(DEFAULT_SYNC_INTERVAL),
            rotate_every: Rotation::Interval(Duration::from_secs(30 * 60)),
        }
    }
}

/// The TOML config file settings
///
/// This struct exists to facilitate the TOML parsing. But the [Config] struct is what we end up
/// generating as public configs that the rest of the application handles.
#[derive(serde::Deserialize)]
struct Raw {
    // Not read, because we do the overlaying at the toml::Table level instead of trying to merge
    // two concrete RawStreamConfig instances.
    #[expect(dead_code)]
    #[serde(default)]
    defaults: RawStreamConfig,
    #[serde(default)]
    interface: HashMap<String, RawStreamConfig>,
}

/// Layer `over` onto `base`: every key in `over` replaces the same key in `base`.
fn overlay(base: &toml::Table, over: &toml::Table) -> toml::Table {
    let mut merged = base.clone();
    for (key, value) in over {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// Parse the given TOML text into a [Raw] config.
///
/// Return also unknown keys, so we can log them.
fn parse_raw(src: &str) -> eyre::Result<(Raw, Vec<String>)> {
    let deserializer = toml::Deserializer::parse(src)?;
    let mut unknown = Vec::new();
    let raw: Raw = serde_ignored::deserialize(deserializer, |path| unknown.push(path.to_string()))?;
    Ok((raw, unknown))
}

impl Config {
    /// Resolve the process configuration from parsed CLI arguments
    pub fn resolve(cli: Cli) -> eyre::Result<Config> {
        match &cli.daemon {
            Some(path) => Config::from_toml_file(path),
            None => Config::from_cli(&cli),
        }
    }

    fn from_cli(cli: &Cli) -> eyre::Result<Config> {
        // A repeated interface would log every frame on it twice, and in --log mode its two Sinks
        // would race to create and truncate the same scheme-named file.
        if let Some(duplicate) = first_duplicate(&cli.interfaces) {
            eyre::bail!("interface {duplicate} given more than once");
        }

        // Compressing stdout would hand a terminal a binary stream, and the pipe case is served just
        // as well by `candumpr can0 | zstd`.
        if cli.compress && !cli.log && cli.output.is_none() {
            eyre::bail!("--compress requires --log or --output");
        }
        if let Some(path) = &cli.output
            && cli.compress
            && path.extension().is_none_or(|ext| ext != "zst")
        {
            // File extensions are useful, but not required. Give a QoL warning.
            tracing::warn!(
                path = %path.display(),
                "output is zstd-compressed but the path does not end in .zst"
            );
        }

        // --log gives every interface its own output, so its traffic lands in its own file.
        // Everything else interleaves all interfaces into one output.
        let outputs: Vec<Output> = if cli.log {
            cli.interfaces
                .iter()
                .map(|interface| Output::Template {
                    dir: ".".into(),
                    interface: interface.clone(),
                    ext: cli.format.ext(cli.compress).to_string(),
                })
                .collect()
        } else if let Some(path) = &cli.output {
            vec![Output::Path(path.clone())]
        } else {
            vec![Output::Stdout]
        };

        let streams = outputs
            .into_iter()
            .map(|output| StreamConfig {
                output,
                format: cli.format,
                timestamp: cli.timestamp,
                compress: cli.compress,
                flush_interval: Some(DEFAULT_FLUSH_INTERVAL),
                sync_interval: Some(DEFAULT_SYNC_INTERVAL),
                rotation: cli.rotate_every,
            })
            .collect();

        Ok(Config {
            interfaces: cli.interfaces.clone(),
            streams,
            retry_activation_failures: false,
        })
    }

    /// Parse a [Config] from the given file path
    pub fn from_toml_file(path: impl AsRef<std::path::Path>) -> eyre::Result<Config> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read config file {}", path.display()))?;
        Config::from_toml(&src).wrap_err_with(|| format!("invalid config file {}", path.display()))
    }

    /// Parse a [Config] from the given TOML text
    pub fn from_toml(src: &str) -> eyre::Result<Config> {
        // First pass: parse the TOML file into a Raw struct. This lets us type-check and provide
        // meaningful errors with correct lines and columns.
        let (raw, unknown) = parse_raw(src)?;
        for key in &unknown {
            tracing::warn!("unknown configuration key: {key:?}");
        }

        let mut interfaces: Vec<String> = raw.interface.keys().cloned().collect();
        interfaces.sort_unstable();
        drop(raw); // just parsed for type-checking and valid TOML syntax. Overlaying is all done on the toml::Table level below.
        eyre::ensure!(
            !interfaces.is_empty(),
            "no [interface.<name>] sections; at least one is required"
        );

        // Second pass: Overlay each interface's table over the [defaults] table and deserialize the
        // result, so absent keys fall through to Settings::default().
        let table: toml::Table = toml::from_str(src)?;
        let defaults = table
            .get("defaults")
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        let sections = table
            .get("interface")
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();

        let mut streams = Vec::with_capacity(interfaces.len());
        for interface in &interfaces {
            // Non-table sections cannot reach here; pass 1 fails on them first.
            let section = sections
                .get(interface)
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            for key in section.keys().filter(|key| defaults.contains_key(*key)) {
                tracing::debug!(interface = %interface, key = %key, "overrides [defaults]");
            }
            let settings: RawStreamConfig = overlay(&defaults, &section).try_into()?;
            let Some(directory) = &settings.directory else {
                eyre::bail!(
                    "interface {interface}: missing `directory` setting in [interface.{interface}] or [defaults]"
                );
            };
            streams.push(StreamConfig {
                output: Output::Template {
                    dir: directory.join(interface),
                    interface: interface.clone(),
                    ext: settings.format.ext(settings.compress).to_string(),
                },
                format: settings.format,
                timestamp: settings.timestamp,
                compress: settings.compress,
                flush_interval: settings.flush_every,
                sync_interval: settings.sync_every,
                rotation: settings.rotate_every,
            });
        }

        Ok(Config {
            interfaces,
            streams,
            retry_activation_failures: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn resolution_layers_interface_over_defaults_over_built_in() {
        let src = r#"
            [defaults]
            directory = "/var/log/can"
            compress = false
            timestamp = "delta"
            sync_every = "off"
            rotate_every = "100MB"

            [interface.can1]

            [interface.can0]
            compress = true
            format = "candump-console"
            flush_every = "250ms"
            rotate_every = "off"
        "#;
        let config = Config::from_toml(src).unwrap();

        assert_eq!(config.interfaces, ["can0", "can1"]);
        assert!(config.retry_activation_failures);
        assert_eq!(
            config.streams,
            [
                StreamConfig {
                    output: Output::Template {
                        dir: "/var/log/can/can0".into(),
                        interface: "can0".to_string(),
                        ext: "txt.zst".to_string(),
                    },
                    format: Format::CandumpConsole,
                    timestamp: TimestampMode::Delta,
                    compress: true,
                    flush_interval: Some(Duration::from_millis(250)),
                    sync_interval: None,
                    // Overrides the inherited size, exactly like `flush_every = "off"` next door.
                    rotation: Rotation::Off,
                },
                StreamConfig {
                    output: Output::Template {
                        dir: "/var/log/can/can1".into(),
                        interface: "can1".to_string(),
                        ext: "log".to_string(),
                    },
                    format: Format::CandumpFile,
                    timestamp: TimestampMode::Delta,
                    compress: false,
                    flush_interval: Some(DEFAULT_FLUSH_INTERVAL),
                    sync_interval: None,
                    rotation: Rotation::Size(100_000_000),
                },
            ]
        );
    }

    #[test]
    fn intervals_accept_durations_and_off() {
        /// Resolve a config whose only interval is `flush_every = <value>`.
        fn flush_every(value: &str) -> eyre::Result<Option<Duration>> {
            let src = format!(
                "[defaults]\ndirectory = \"/x\"\nflush_every = {value:?}\n[interface.can0]\n"
            );
            Ok(Config::from_toml(&src)?.streams[0].flush_interval)
        }

        assert_eq!(flush_every("5s").unwrap(), Some(Duration::from_secs(5)));
        assert_eq!(
            flush_every("500ms").unwrap(),
            Some(Duration::from_millis(500))
        );
        assert_eq!(flush_every("5min").unwrap(), Some(Duration::from_secs(300)));
        assert_eq!(
            flush_every("1min 30s").unwrap(),
            Some(Duration::from_secs(90))
        );
        assert_eq!(flush_every("off").unwrap(), None);

        let err = format!("{:#}", flush_every("soon").unwrap_err());
        assert!(err.contains("a duration like"), "got: {err}");
        let err = format!("{:#}", flush_every("-3s").unwrap_err());
        assert!(err.contains("must not be negative"), "got: {err}");
        let err = format!("{:#}", flush_every("64KB").unwrap_err());
        assert!(err.contains("not yet supported"), "got: {err}");
    }

    #[test]
    fn rotation_tells_sizes_and_durations_apart() {
        fn rotation(value: &str) -> Result<Rotation, String> {
            value.parse()
        }
        fn size(bytes: u64) -> Result<Rotation, String> {
            Ok(Rotation::Size(bytes))
        }
        fn secs(secs: u64) -> Result<Rotation, String> {
            Ok(Rotation::Interval(Duration::from_secs(secs)))
        }

        // The two that pin the discriminator: a trailing `b` is the only difference.
        assert_eq!(rotation("1m"), secs(60));
        assert_eq!(rotation("1MB"), size(1_000_000));

        assert_eq!(rotation("100MB"), size(100_000_000));
        assert_eq!(rotation("100MiB"), size(104_857_600));
        assert_eq!(rotation("1.5GB"), size(1_500_000_000));
        assert_eq!(rotation("100 mb"), size(100_000_000));
        assert_eq!(rotation("512b"), size(512));

        assert_eq!(rotation("30min"), secs(1800));
        assert_eq!(rotation("1h"), secs(3600));
        assert_eq!(rotation("90s"), secs(90));
        assert_eq!(rotation("1min 30s"), secs(90));
        assert_eq!(rotation("1 day"), secs(24 * 3600));
        assert_eq!(rotation("2 weeks"), secs(24 * 3600 * 7 * 2));

        assert_eq!(rotation("off"), Ok(Rotation::Off));
        assert_eq!(rotation("OFF"), Ok(Rotation::Off));

        // A bare number is a forgotten unit, not 100 bytes, so the error names both forms.
        let err = rotation("100").unwrap_err();
        assert!(err.contains("expected a size like"), "got: {err}");
        let err = rotation("soon").unwrap_err();
        assert!(err.contains("expected a size like"), "got: {err}");

        // Sizes have a floor, not just a nonzero check. 100 bytes is the first accepted value.
        assert_eq!(rotation("100b"), size(100));
        let err = rotation("99b").unwrap_err();
        assert!(err.contains("must be bigger than"), "got: {err}");
        let err = rotation("0MB").unwrap_err();
        assert!(err.contains("must be bigger than"), "got: {err}");
        assert!(
            rotation("0s")
                .unwrap_err()
                .contains("duration must not be zero")
        );
        assert!(
            rotation("-3s")
                .unwrap_err()
                .contains("duration must not be negative")
        );
        let err = rotation("10 files").unwrap_err();
        assert!(err.contains("cannot be a file count"), "got: {err}");
    }

    #[test]
    fn resolution_rejects_incomplete_configs() {
        let err = format!(
            "{:#}",
            Config::from_toml("[defaults]\ncompress = true\n[interface.can0]\n").unwrap_err()
        );
        assert!(
            err.contains("missing `directory` setting in [interface.can0] or [defaults]"),
            "got: {err}"
        );

        let err = format!(
            "{:#}",
            Config::from_toml("[defaults]\ndirectory = \"/x\"\n").unwrap_err()
        );
        assert!(err.contains("at least one is required"), "got: {err}");

        let err = format!("{:#}", Config::from_toml("[interface]\n").unwrap_err());
        assert!(err.contains("at least one is required"), "got: {err}");
    }
}
