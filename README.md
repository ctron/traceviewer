# traceviewer

[![CI](https://github.com/ctron/traceview/actions/workflows/ci.yml/badge.svg)](https://github.com/ctron/traceview/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/traceviewer.svg)](https://crates.io/crates/traceviewer)

`traceviewer` runs a command and shows its output in a scrollable terminal log
viewer.

It is useful for inspecting long-running command output while keeping recent
log lines selectable, searchable by eye, and copyable from inside the terminal.

## Install

Install a prebuilt binary with `cargo-binstall`:

```sh
cargo binstall traceviewer
```

Or build from source with Cargo:

```sh
cargo install traceviewer
```

The installed binary is named `tv`.

## Usage

```sh
tv -- cargo test
tv --format tracing -- my-service
tv --format bunyan -- node service.js
tv --max-lines 5000 -- ./run.sh
```

By default, `traceviewer` attempts to recognize supported formats automatically.
Use `--format` to choose one explicitly:

- `auto`: detect supported formats
- `plain`: show lines without parsing
- `bunyan`: Bunyan JSON records
- `env-logger`: common `env_logger` text output
- `tracing`: common `tracing` text output

## Controls

```text
Up / Down       move cursor one line
PgUp / PgDown   move cursor one page
Home / End      jump to first or last retained line
Left / Right    scroll horizontally
f               focus selected target, or clear focus
s               toggle span information
y               copy selected line to clipboard
?               toggle help
q / Esc         exit after the process ends
Ctrl-C          kill process and exit
```

## License

This project is licensed under the terms in `LICENSE`.
