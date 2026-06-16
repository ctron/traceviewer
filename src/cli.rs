use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Run a command and inspect its logs in a scrollable terminal viewer",
    trailing_var_arg = true
)]
pub(crate) struct Cli {
    /// Log parser to use. Auto currently recognizes bunyan, env_logger, and tracing fmt defaults.
    #[arg(short, long, value_enum, default_value_t = LogFormat::Auto)]
    pub(crate) format: LogFormat,

    /// Optional maximum number of log lines to keep in memory. By default the buffer is unbounded.
    #[arg(long)]
    pub(crate) max_lines: Option<usize>,

    /// Command to run, followed by its arguments. Use `--` before the command when needed.
    #[arg(required = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LogFormat {
    Auto,
    Bunyan,
    Plain,
    EnvLogger,
    Tracing,
}
