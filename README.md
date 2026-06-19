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
tv --format logfmt -- ./service
tv --file app.log
```

Supported formats:

- `auto`: detect supported formats
- `plain`: show lines without parsing
- `bunyan`: Bunyan JSON records
- `env-logger`: common `env_logger` text output
- `logfmt`: key=value records
- `tracing`: common `tracing` text output

Run `tv --help` for available options.

## Showcase Example

The repository includes a small example app that emits representative log lines
for each supported parser:

```sh
cargo run --example showcase -- env-logger
cargo run --example showcase -- logfmt
cargo run --example showcase -- tracing
cargo run --example showcase -- bunyan
```

Run it through `tv` to inspect the rendering:

```sh
cargo run --bin tv -- --format env-logger -- cargo run --example showcase -- env-logger
cargo run --bin tv -- --format logfmt -- cargo run --example showcase -- logfmt
cargo run --bin tv -- --format tracing -- cargo run --example showcase -- tracing
cargo run --bin tv -- --format bunyan -- cargo run --example showcase -- bunyan
```

## License

This project is licensed under the terms in `LICENSE`.
