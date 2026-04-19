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

| Flag                  | Default                                   | Description                                                                                           |
| --------------------- | ----------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `<INTERFACES...>`     | required                                  | CAN interfaces with optional candump-style inline filters (e.g. `can0,0x18FE:0x1FFF`)                 |
| `-l`                  | false                                     | Log to file using the default filename template in the current working directory                      |
| `-o <TEMPLATE>`       | {interface}_{index}_{timestamp-iso}.{ext} | Output file path template (implies `-l`). Supports placeholders                                       |
| `--format <FMT>`      | `candump-file`                            | Output format: `candump-file`, `candump-console`, `asc`, `pcap`                                       |
| `--compress`          | false                                     | Enable zstd compression.                                                                              |
| `--timestamp <MODE>`  | `absolute`                                | Timestamp mode: `absolute`, `delta`, `zero`. Only applied to candump-file and candump-console formats |
| `--address-claim`     | false                                     | Send J1939 address claim PGN request on start (applies to all interfaces)                             |
| `--no-error-frames`   | false                                     | Disable error frame logging (error frames are logged by default)                                      |
| `--batch-size <N>`    | `auto`                                    | io_uring batch size. `auto` uses a small batch for stdout, larger for file output                     |
| `--log-level <LEVEL>` | `INFO`                                    | Stderr log level                                                                                      |

### Multi-interface file output

If the output path template does not contain `{interface}`, all traffic from all interfaces is
written to a single file. All output formats support interleaved multi-interface traffic. Otherwise,
there's a per-interface file created.

This is supported regardless of daemon or CLI mode.

## Daemon mode

```
candumpr --daemon=config.toml [--log-level=LEVEL]
```

`--daemon` is mutually exclusive with all other arguments except `--log-level`.

# Filename templates

File output paths support the following placeholders:

| Placeholder        | Resolves to                                                                      |
| ------------------ | -------------------------------------------------------------------------------- |
| `{interface}`      | CAN interface name (e.g. `can0`)                                                 |
| `{timestamp-iso}`  | ISO 8601-ish timestamp from the first frame, does not use colons (compatibility) |
| `{timestamp-unix}` | Unix timestamp (seconds) from the first frame                                    |
| `{index}`          | Monotonically increasing file index                                              |
| `{ext}`            | File extension based on format and compression                                   |

The default template is:

```
{interface}_{index}_{timestamp-iso}.{ext}
```

## Deferred file creation

Log files are not created until the first frame is received on an interface. The `{timestamp-iso}`
and `{timestamp-unix}` placeholders resolve from the first frame's timestamp, not from when candumpr
started. This has two benefits:

* The filename reflects when traffic actually started, not when the process launched.
* On systems where the RTC is unset at boot, the frame's timestamp (which may come from a valid
  hardware source even when the system clock is wrong) produces a more meaningful filename.

The receiver is responsible for providing a timestamp on every frame, falling back to a software
timestamp if hardware timestamps are not available. The filename timestamp always comes from
whatever the receiver provides.

No empty log files are created for interfaces that never see traffic.

## Index persistence

The `{index}` placeholder provides log ordering in the absence of a reliable system clock. When
candumpr starts, it scans the output directory for existing files matching the template pattern and
picks the next available index. This makes the index persistent across restarts.

## Path handling

The template may be a relative path (relative to the candumpr process's working directory) or an
absolute path. Directories are created as needed.

Example absolute path template:

```
/var/log/raw/{interface}/{index}_{interface}.{ext}
```

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
path = "/var/log/can/{index}_{interface}_{timestamp-iso}.{ext}"
batch_size = "auto"
rcvbuf = 212992
address_claim = false
error_frames = true

[defaults.rotation]
limit = "100MB"

[defaults.retention]
limit = "1GB"

[interface.can0]
filters = ["0x18FE:0x1FFF", "0x100~0x7FF"]
address_claim = true

[interface.can0.retention]
limit = "500MB"

[interface.can1]
filters = ["0x200:0x7FF"]
```

## Defaults section

The `[defaults]` section provides base values that all interfaces inherit from. Any option set in an
`[interface.<name>]` section overrides the corresponding default.

| Key             | Type              | Default                                       | Description                                                                            |
| --------------- | ----------------- | --------------------------------------------- | -------------------------------------------------------------------------------------- |
| `format`        | string            | `"candump-file"`                              | Output format: `"candump-file"`, `"candump-console"`, `"asc"`, `"pcap"`                |
| `compress`      | boolean           | `true`                                        | Enable zstd compression                                                                |
| `timestamp`     | string            | `"absolute"`                                  | Timestamp mode: `"absolute"`, `"delta"`, `"zero"`. Only applies to the candump formats |
| `path`          | string            | `"{index}_{interface}_{timestamp-iso}.{ext}"` | Output file path template                                                              |
| `batch_size`    | string or integer | `"auto"`                                      | io_uring batch size                                                                    |
| `rcvbuf`        | integer           | system default                                | Socket receive buffer size in bytes                                                    |
| `address_claim` | boolean           | `false`                                       | Send J1939 address claim PGN request on rotation                                       |
| `error_frames`  | boolean           | `true`                                        | Log error frames                                                                       |
| `filters`       | array of strings  | `[]`                                          | candump-style filters                                                                  |

## Rotation

Configured under `[defaults.rotation]` or `[interface.<name>.rotation]`.

| Key     | Type   | Default  | Description                                                        |
| ------- | ------ | -------- | ------------------------------------------------------------------ |
| `limit` | string | required | Rotation trigger. Size (e.g. `"100MB"`) or duration (e.g. `"1h"`). |

Size and duration are mutually exclusive: the limit is either a size or a duration, not both.

SIGHUP always triggers an immediate rotation regardless of the configured limit.

## Retention

Configured under `[defaults.retention]` or `[interface.<name>.retention]`.

| Key     | Type   | Default  | Description                                         |
| ------- | ------ | -------- | --------------------------------------------------- |
| `limit` | string | required | Retention limit per interface. Size (e.g. `"1GB"`). |

Retention is enforced per-interface. Each interface independently manages its own log files and
stays within its own limit. If total disk usage across all interfaces must be bounded, the user
allocates budget across interfaces based on their knowledge of the traffic patterns.

There is no global retention policy for the aggregate data used by all interfaces.

## Interface sections

Each `[interface.<name>]` section configures a specific CAN interface. The interface name is the
Linux network interface name (e.g. `can0`, `vcan0`).

All keys from `[defaults]` can be overridden per-interface, plus the rotation and retention
subsections. At least one `[interface.<name>]` section must be present.

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
