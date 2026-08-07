# candumpr configuration

candumpr has two modes: interactive CLI mode, and daemon mode. The `--daemon` CLI argument is
incompatible with all other CLI arguments (building an overlay system for CLI and the config file
was too daunting to build).

```sh
candumpr --daemon=path/to/config.toml
```

## Example 1: minimum required configs

```toml
[defaults]
directory = "/var/log/can"

[interface.can0]
[interface.can1]
```

## Example 2: overriding defaults

You can specify default values for the `[interface.<name>]` tables in the `[defaults]` table. The
following example logs uncompressed data for `can0`, but compresses `can1`.

```toml
[defaults]
directory = "/var/log/can"
compress = false

[interface.can0]
# take all of the default values

[interface.can1]
compress = true
```

## Interface table keys

Each of the `[defaults]` and `[interface.<name>]` tables support the following keys:

| Key                      | Default          | Description                                                                                            |
| ------------------------ | ---------------- | ------------------------------------------------------------------------------------------------------ |
| `directory`              | required         | Directory to log in. Each interface will log to `<directory>/<interface>/`                             |
| `format`                 | `"candump-file"` | One of: `"candump-file"` or `"candump-console"`                                                        |
| `compress`               | `true`           | Compress the log files with zstd                                                                       |
| `timestamp`              | `"absolute"`     | Timestamp mode: `"absolute"`, `"delta"`, or `"zero"`                                                   |
| `flush_every`            | `"5s"`           | Periodic flush interval: a time duration or a size                                                     |
| `sync_every`             | `"5min"`         | Periodic fsync interval: a time duration or a size                                                     |
| `rotate_every`           | `"30min"`        | Rotate the log once it exceeds a size or an age                                                        |
| `retain`                 | `"1 GB"`         | Delete the oldest log files once the interface directory exceeds a total size, an age, or a file count |
| `request_address_claims` | `true`           | Broadcast a J1939 Address Claim PGN request on the interface whenever one of its log files is opened   |

Durations are parsed using
[jiff's friendly format](https://docs.rs/jiff/latest/jiff/fmt/friendly/index.html). Days and weeks
are also accepted.

The `rotate_every`, `flush_every`, and `sync_every` parameters accept durations or sizes. Examples:
`512B`, `100 MiB`, `1 gb`, `1m` (1 minute), `30 minutes`, `1 day`, `off`.

The `retain` parameter additionally accepts file counts, like `"10 files"`.

## Output files

Each interface logs to its own subdirectory using files with the following format:

```
<directory>/<interface>/i<index>_<interface>_<timestamp>.<ext>
```
