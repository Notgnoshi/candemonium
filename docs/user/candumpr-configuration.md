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

| Key              | Default          | Description                                                                |
| ---------------- | ---------------- | -------------------------------------------------------------------------- |
| `directory`      | required         | Directory to log in. Each interface will log to `<directory>/<interface>/` |
| `format`         | `"candump-file"` | One of: `"candump-file"` or `"candump-console"`                            |
| `compress`       | `true`           | Compress the log files with zstd                                           |
| `timestamp`      | `"absolute"`     | Timestamp mode: `"absolute"`, `"delta"`, or `"zero"`                       |
| `flush_interval` | `"5s"`           | Upper bound between flushes. `"off"` disables periodic flushing            |
| `sync_interval`  | `"5min"`         | Upper bound between fsyncs. `"off"` disables periodic syncing              |

The interval strings are parsed using
[jiff::SignedDuration](https://docs.rs/jiff/latest/jiff/struct.SignedDuration.html).

## Output files

Each interface logs to its own subdirectory using files with the following format:

```
<directory>/<interface>/i<index>_<interface>_<timestamp>.<ext>
```
