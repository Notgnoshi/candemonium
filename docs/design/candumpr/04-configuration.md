# candumpr configuration

Status: **DRAFT**

# Scope

This document sketches out the CLI and config file configuration options for the candumpr features
described in [01-goals.md](/docs/design/candumpr/01-goals.md).

# Design principles

candumpr has two use-cases that drive the configuration design:

1. **Logging daemon**: a long-running process that logs CAN traffic to disk with rotation and
   retention policies. Configured via a TOML config file.
2. **Live troubleshooting**: a developer runs candumpr in the console to inspect CAN traffic.
   Configured via CLI arguments.

The config file is only read when `--daemon=config.toml` is passed. There is no default config file
path. This is how we distinguish the two use-cases: if you are running candumpr interactively, you
use CLI arguments. If you are running it as a daemon, you use a config file.

The CLI arguments are a subset of the config file options. The CLI does not expose rotation,
retention, rcvbuf, or per-interface configuration. It targets the live troubleshooting use-case.

# CLI arguments

## Non-daemon mode

```
candumpr [OPTIONS] <INTERFACES...>
```

| Flag                  | Default        | Description                                                                                           |
| --------------------- | -------------- | ----------------------------------------------------------------------------------------------------- |
| `<INTERFACES...>`     | required       | CAN interfaces with optional candump-style inline filters (e.g. `can0,0x18FE:0x1FFF`)                 |
| `-l`                  | false          | Log each interface to its own file in the current working directory, named by the fixed scheme        |
| `-o <FILE>`           | none           | Log all interfaces interleaved into exactly this file. Mutually exclusive with `-l`                   |
| `--format <FMT>`      | `candump-file` | Output format: `candump-file`, `candump-console`, `asc`, `pcap`                                       |
| `--compress`          | false          | Enable zstd compression.                                                                              |
| `--timestamp <MODE>`  | `absolute`     | Timestamp mode: `absolute`, `delta`, `zero`. Only applied to candump-file and candump-console formats |
| `--address-claim`     | false          | Send J1939 address claim PGN request on start (applies to all interfaces)                             |
| `--no-error-frames`   | false          | Disable error frame logging (error frames are logged by default)                                      |
| `--batch-size <N>`    | `auto`         | io_uring batch size. `auto` uses a small batch for stdout, larger for file output                     |
| `--log-level <LEVEL>` | `INFO`         | Stderr log level                                                                                      |

### Multi-interface file output

`-l` always creates one file per interface. `-o` writes all traffic from all interfaces interleaved
into a single file; all output formats support interleaved multi-interface traffic. Daemon mode is
always per-interface.

## Daemon mode

```
candumpr --daemon=config.toml [--log-level=LEVEL]
```

`--daemon` is mutually exclusive with all other arguments except `--log-level`.

# File naming

There is no user-visible filename templating. candumpr names log files with a fixed scheme:

```
i<index>_<interface>_<timestamp>.<ext>                            CLI mode (-l), in the working directory
<directory>/<interface>/i<index>_<interface>_<timestamp>.<ext>    daemon mode
```

| Field         | Resolves to                                                          |
| ------------- | -------------------------------------------------------------------- |
| `<interface>` | CAN interface name (e.g. `can0`)                                     |
| `<timestamp>` | ISO 8601-ish UTC timestamp from the first frame, does not use colons |
| `<index>`     | Monotonically increasing file index, zero-padded to width 4          |
| `<ext>`       | File extension based on format and compression                       |

Files named with `-o` are exempt from the scheme; the given name is used verbatim, and the file is
truncated if it already exists.

## Deferred file creation

Log files are not created until the first frame is received on an interface. The filename timestamp
resolves from the first frame's timestamp, not from when candumpr started. This has two benefits:

* The filename reflects when traffic actually started, not when the process launched.
* On systems where the RTC is unset at boot, the frame's timestamp (which may come from a valid
  hardware source even when the system clock is wrong) produces a more meaningful filename.

The receiver is responsible for providing a timestamp on every frame, falling back to a software
timestamp if hardware timestamps are not available. The filename timestamp always comes from
whatever the receiver provides.

No empty log files are created for interfaces that never see traffic.

## Index persistence

The filename index provides log ordering in the absence of a reliable system clock. When candumpr
starts, it scans the output directory for existing files matching the naming scheme and picks the
next available index. This makes the index persistent across restarts. `-o` files contain no index
and are never scanned or matched.

## Path handling

The daemon `directory` and the `-o` file may be relative (to the candumpr process's working
directory) or absolute paths. Directories are created as needed, including the per-interface
subdirectories in daemon mode.

# Config file

The config file is TOML. It has a `[defaults]` section that provides base configuration, and
`[interface.<name>]` sections that override defaults for specific interfaces. At least one interface
must be configured.

## Example

```toml
[defaults]
format = "candump-file"
compress = true
timestamp = "absolute"
directory = "/var/log/can"
batch_size = "auto"
rcvbuf = 212992
address_claim = false
error_frames = true
rotate_every = "100MB"
retain = "1GB"

[interface.can0]
filters = ["0x18FE:0x1FFF", "0x100~0x7FF"]
address_claim = true
retain = "500MB"

[interface.can1]
filters = ["0x200:0x7FF"]
```

## Defaults section

The `[defaults]` section provides base values that all interfaces inherit from. Any option set in an
`[interface.<name>]` section overrides the corresponding default.

| Key             | Type              | Default          | Description                                                                                  |
| --------------- | ----------------- | ---------------- | -------------------------------------------------------------------------------------------- |
| `format`        | string            | `"candump-file"` | Output format: `"candump-file"`, `"candump-console"`, `"asc"`, `"pcap"`                      |
| `compress`      | boolean           | `true`           | Enable zstd compression                                                                      |
| `timestamp`     | string            | `"absolute"`     | Timestamp mode: `"absolute"`, `"delta"`, `"zero"`. Only applies to the candump formats       |
| `directory`     | string            | required         | Directory to log in. Each interface logs to `<directory>/<interface>/`                       |
| `batch_size`    | string or integer | `"auto"`         | io_uring batch size                                                                          |
| `rcvbuf`        | integer           | system default   | Socket receive buffer size in bytes                                                          |
| `address_claim` | boolean           | `false`          | Send J1939 address claim PGN request on rotation                                             |
| `error_frames`  | boolean           | `true`           | Log error frames                                                                             |
| `filters`       | array of strings  | `[]`             | candump-style filters                                                                        |
| `flush_every`   | string            | `"5s"`           | Flush interval: a duration or a size. `"off"` disables automatic flush                       |
| `sync_every`    | string            | `"5min"`         | Sync interval: a duration or a size. `"off"` disables periodic sync                          |
| `rotate_every`  | string            | `"30min"`        | Rotation trigger: a size (e.g. `"100MB"`) or a duration (e.g. `"1h"`). `"off"` disables      |
| `retain`        | string            | `"1 GB"`         | Retention limit: a total size, an age, or a file count (e.g. `"10 files"`). `"off"` disables |

## Rotation

Rotation is configured with the `rotate_every` key. The limit is either a size or a duration, not
both.

SIGHUP always triggers an immediate rotation regardless of the configured limit.

## Retention

Retention is configured with the `retain` key: a total directory size, a file age, or a file count.

Retention is enforced per-interface. Each interface independently manages its own log files and
stays within its own limit. If total disk usage across all interfaces must be bounded, the user
allocates budget across interfaces based on their knowledge of the traffic patterns.

There is no global retention policy for the aggregate data used by all interfaces.

## Interface sections

Each `[interface.<name>]` section configures a specific CAN interface. The interface name is the
Linux network interface name (e.g. `can0`, `vcan0`).

All keys from `[defaults]` can be overridden per-interface. At least one `[interface.<name>]`
section must be present.

# Filter syntax

Filters use the candump filter syntax for compatibility:

```
<can_id>:<can_mask>      include filter
<can_id>~<can_mask>      exclude filter
```

On the CLI, filters are specified inline with the interface name, comma-separated:

```
candumpr can0,0x18FE:0x1FFF,0x100~0x7FF can1
```

This is a boolean OR. Append `j` or `J` to "join" the filters (boolean AND).

In the config file, filters are an array of strings:

```toml
[interface.can0]
filters = ["0x18FE:0x1FFF", "0x100~0x7FF"]
```
