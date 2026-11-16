# harness-cli

The `harness` binary. In later phases it wires adapters into the runtime and
hosts the commands from Tech spec 3.16. In Phase 0 it parses `--help` /
`--version`, sets up logging and prints help.

## Public API (library half)

| Item | Purpose |
|---|---|
| `telemetry::filter_from` | Parses `HARNESS_LOG` (default `info`) |
| `telemetry::file_appender` | `harness.YYYY-MM-DD.log`, rotated daily, 7 files kept |
| `telemetry::init` | Installs the global subscriber; keep the returned guard alive |

## Usage

```sh
harness --version
HARNESS_LOG=harness=debug,info harness
tail -f ~/.local/share/harness/logs/harness.*.log
```

## Design notes

- Logs never go to stdout or stderr, because the TUI (Phase 4) owns the terminal.
- The end-to-end tests run the real binary with `HOME` pointed at a temp dir,
  so they never touch the user's files.
