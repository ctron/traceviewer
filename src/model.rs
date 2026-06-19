use std::process::ExitStatus;

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub(crate) fn indicator(self) -> char {
        match self {
            Self::Stdout => '|',
            Self::Stderr => '!',
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Unknown,
}

impl Level {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Unknown => "",
        }
    }

    pub(crate) fn severity(self) -> u8 {
        match self {
            Self::Error => 5,
            Self::Warn => 4,
            Self::Info => 3,
            Self::Debug => 2,
            Self::Trace => 1,
            Self::Unknown => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LogEntry {
    pub(crate) raw: String,
    pub(crate) timestamp: Option<String>,
    pub(crate) level: Level,
    pub(crate) parsed: bool,
    pub(crate) target: Option<String>,
    pub(crate) spans: Vec<String>,
    pub(crate) values: Vec<TraceValueSection>,
    pub(crate) message: String,
    pub(crate) message_parts: Vec<MessagePart>,
    pub(crate) stream: Stream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TraceValueSection {
    pub(crate) title: String,
    pub(crate) fields: Vec<TraceValueField>,
}

impl TraceValueSection {
    pub(crate) fn new(title: impl Into<String>, fields: Vec<TraceValueField>) -> Self {
        Self {
            title: title.into(),
            fields,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TraceValueField {
    pub(crate) key: String,
    pub(crate) value: TraceValue,
}

impl TraceValueField {
    pub(crate) fn new(key: impl Into<String>, value: TraceValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TraceValue {
    Bool(bool),
    Null,
    Number(String),
    String(String),
    Object(Vec<(String, TraceValue)>),
    Array(Vec<TraceValue>),
    Other(String),
}

impl TraceValue {
    pub(crate) fn from_tracing_text(value: &str) -> Self {
        if let Ok(value) = serde_json::from_str(value) {
            Self::from_json(value)
        } else if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
            Self::Number(value.to_string())
        } else {
            Self::Other(value.to_string())
        }
    }

    pub(crate) fn from_json(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(value),
            Value::Number(value) => Self::Number(value.to_string()),
            Value::String(value) => Self::String(value),
            Value::Array(values) => Self::Array(values.into_iter().map(Self::from_json).collect()),
            Value::Object(fields) => Self::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, Self::from_json(value)))
                    .collect(),
            ),
        }
    }

    pub(crate) fn render_text(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Null => "null".to_string(),
            Self::Number(value) | Self::Other(value) => value.clone(),
            Self::String(value) => {
                serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
            }
            Self::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::render_text)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                            value.render_text()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MessagePart {
    Text(String),
    Fields(Vec<TraceValueField>),
}

impl MessagePart {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub(crate) fn fields(fields: Vec<TraceValueField>) -> Self {
        Self::Fields(fields)
    }

    pub(crate) fn plain_text(parts: &[Self]) -> String {
        let mut text = String::new();
        for part in parts {
            match part {
                Self::Text(part) => text.push_str(part),
                Self::Fields(fields) => {
                    if text.is_empty() {
                        text.push('(');
                    } else {
                        text.push_str(" (");
                    }
                    push_fields_text(&mut text, fields);
                    text.push(')');
                }
            }
        }
        text
    }
}

fn push_fields_text(text: &mut String, fields: &[TraceValueField]) {
    for (idx, field) in fields.iter().enumerate() {
        if idx > 0 {
            text.push(' ');
        }
        text.push_str(&field.key);
        text.push('=');
        text.push_str(&field.value.render_text());
    }
}

#[derive(Debug)]
pub(crate) enum AppEvent {
    Line(Stream, String),
    InputFinished,
    ProcessExited(ExitStatus),
    ReaderFailed(Stream, String),
}
