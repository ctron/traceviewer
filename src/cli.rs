use std::{num::NonZeroUsize, path::PathBuf};

use bytesize::ByteSize;
use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Run a command and inspect its logs in a scrollable terminal viewer",
    trailing_var_arg = true
)]
pub(crate) struct Cli {
    /// Log parser to use. Auto currently recognizes bunyan, env_logger, logfmt, and tracing fmt defaults.
    #[arg(
        short,
        long,
        value_enum,
        default_value_t = LogFormat::Auto,
        env = "TRACEVIEWER_FORMAT"
    )]
    pub(crate) format: LogFormat,

    /// Optional maximum number of log lines to keep in memory. By default the buffer is unbounded.
    #[arg(long, env = "TRACEVIEWER_MAX_LINES")]
    pub(crate) max_lines: Option<NonZeroUsize>,

    /// Maximum bytes retained from a single log line. Accepts values like `65536` or `64KiB`.
    #[arg(
        long,
        default_value = "64KiB",
        env = "TRACEVIEWER_MAX_LINE_BYTES",
        value_parser = parse_byte_size
    )]
    pub(crate) max_line_bytes: NonZeroUsize,

    /// Read log lines from a file instead of running a command.
    #[arg(long, short = 'i', conflicts_with = "command")]
    pub(crate) file: Option<PathBuf>,

    /// Command to run, followed by its arguments. Use `--` before the command when needed.
    #[arg(required_unless_present = "file")]
    pub(crate) command: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LogFormat {
    Auto,
    Bunyan,
    Plain,
    EnvLogger,
    Logfmt,
    Tracing,
}

fn parse_byte_size(value: &str) -> Result<NonZeroUsize, String> {
    let bytes = value
        .parse::<ByteSize>()
        .map_err(|err| format!("invalid byte size: {err}"))?
        .as_u64();
    let bytes = usize::try_from(bytes)
        .map_err(|_| "byte size is too large for this platform".to_string())?;
    NonZeroUsize::new(bytes).ok_or_else(|| "byte size must be greater than zero".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_max_line_bytes() {
        let cli =
            Cli::try_parse_from(["tv", "--max-line-bytes", "65536", "true"]).expect("parse cli");

        assert_eq!(cli.max_line_bytes.get(), 65_536);
    }

    #[test]
    fn parses_default_max_line_bytes() {
        let cli = Cli::try_parse_from(["tv", "true"]).expect("parse cli");

        assert_eq!(cli.max_line_bytes.get(), 65_536);
    }

    #[test]
    fn parses_human_max_line_bytes() {
        let cli =
            Cli::try_parse_from(["tv", "--max-line-bytes", "10KiB", "true"]).expect("parse cli");

        assert_eq!(cli.max_line_bytes.get(), 10_240);
    }

    #[test]
    fn rejects_zero_max_line_bytes() {
        let err = Cli::try_parse_from(["tv", "--max-line-bytes", "0 B", "true"])
            .expect_err("zero limit should fail");

        assert!(
            err.to_string()
                .contains("byte size must be greater than zero")
        );
    }

    #[test]
    fn accepts_file_without_command() {
        let cli = Cli::try_parse_from(["tv", "--file", "debug/long-line.txt"]).expect("parse cli");

        assert_eq!(
            cli.file.as_deref(),
            Some(std::path::Path::new("debug/long-line.txt"))
        );
        assert!(cli.command.is_empty());
    }

    #[test]
    fn rejects_file_with_command() {
        let err = Cli::try_parse_from(["tv", "--file", "debug/long-line.txt", "true"])
            .expect_err("file and command should conflict");

        assert!(err.to_string().contains("cannot be used with"));
    }
}
