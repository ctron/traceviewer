use crate::{
    cli::LogFormat,
    model::{Level, LogEntry, MessagePart, Stream, TraceValue, TraceValueField, TraceValueSection},
};
use serde_json::{Map, Value};

const BUNYAN_CORE_FIELDS: &[&str] = &["name", "hostname", "pid", "level", "msg", "time", "v"];
const MAX_TRACING_FIELD_PARSE_STEPS: usize = 1024;

pub(crate) fn parse_log_line(format: LogFormat, stream: Stream, raw: String) -> LogEntry {
    let raw = strip_ansi_escape_sequences(&raw);
    let parsed = match format {
        LogFormat::Auto => parse_bunyan(&raw, stream)
            .or_else(|| parse_tracing(&raw))
            .or_else(|| parse_logfmt(&raw, stream))
            .or_else(|| parse_env_logger(&raw)),
        LogFormat::Bunyan => parse_bunyan(&raw, stream),
        LogFormat::Plain => None,
        LogFormat::EnvLogger => parse_env_logger(&raw),
        LogFormat::Logfmt => parse_logfmt(&raw, stream),
        LogFormat::Tracing => parse_tracing(&raw),
    };

    parsed.unwrap_or_else(|| LogEntry {
        raw: raw.clone(),
        level: if stream == Stream::Stderr {
            Level::Warn
        } else {
            Level::Unknown
        },
        timestamp: None,
        target: None,
        spans: Vec::new(),
        values: Vec::new(),
        message: raw.clone(),
        message_parts: Vec::new(),
        parsed: false,
        stream,
    })
}

fn strip_ansi_escape_sequences(value: &str) -> String {
    let mut stripped = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            stripped.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut previous_was_escape = false;
                for ch in chars.by_ref() {
                    if ch == '\x07' || (previous_was_escape && ch == '\\') {
                        break;
                    }
                    previous_was_escape = ch == '\x1b';
                }
            }
            _ => {}
        }
    }

    stripped
}

fn parse_env_logger(raw: &str) -> Option<LogEntry> {
    let raw = raw.trim_end();
    let rest = raw.strip_prefix('[')?;
    let close = rest.find(']')?;
    let header = &rest[..close];
    let message = rest[close + 1..].trim_start();
    let fields: Vec<_> = header.split_whitespace().collect();
    if fields.len() < 2 {
        return None;
    }

    let level_pos = fields
        .iter()
        .position(|field| parse_level(field).is_some())?;
    let level = parse_level(fields[level_pos])?;
    let timestamp = if level_pos > 0 {
        Some(fields[..level_pos].join(" "))
    } else {
        None
    };
    let target = if fields.len() > level_pos + 1 {
        Some(fields[level_pos + 1..].join(" "))
    } else {
        None
    };

    Some(LogEntry {
        raw: raw.to_string(),
        timestamp,
        level,
        parsed: true,
        target,
        spans: Vec::new(),
        values: Vec::new(),
        message: message.to_string(),
        message_parts: Vec::new(),
        stream: Stream::Stdout,
    })
}

fn parse_logfmt(raw: &str, stream: Stream) -> Option<LogEntry> {
    let raw = raw.trim_end();
    let fields = parse_logfmt_fields(raw)?;
    if fields.is_empty() {
        return None;
    }

    let level = fields
        .iter()
        .find(|field| field.key == "level" || field.key == "lvl")
        .and_then(|field| parse_level(&logfmt_core_text(&field.value)))
        .unwrap_or(Level::Unknown);
    let timestamp = fields
        .iter()
        .find(|field| matches!(field.key.as_str(), "time" | "timestamp" | "ts"))
        .map(|field| logfmt_core_text(&field.value));
    let target = fields
        .iter()
        .find(|field| matches!(field.key.as_str(), "target" | "logger" | "module"))
        .map(|field| logfmt_core_text(&field.value));
    let message = fields
        .iter()
        .find(|field| matches!(field.key.as_str(), "msg" | "message"))
        .map(|field| logfmt_core_text(&field.value))
        .unwrap_or_default();
    let values: Vec<_> = fields
        .iter()
        .filter(|field| !LOGFMT_CORE_FIELDS.contains(&field.key.as_str()))
        .cloned()
        .collect();
    let message_parts = if values.is_empty() {
        if message.is_empty() {
            Vec::new()
        } else {
            vec![MessagePart::text(&message)]
        }
    } else {
        let mut parts = Vec::new();
        if !message.is_empty() {
            parts.push(MessagePart::text(&message));
        }
        parts.push(MessagePart::fields(values.clone()));
        parts
    };
    let message = MessagePart::plain_text(&message_parts);
    let values = if values.is_empty() {
        Vec::new()
    } else {
        vec![TraceValueSection::new("event", values)]
    };

    Some(LogEntry {
        raw: raw.to_string(),
        timestamp,
        level,
        parsed: true,
        target,
        spans: Vec::new(),
        values,
        message,
        message_parts,
        stream,
    })
}

const LOGFMT_CORE_FIELDS: &[&str] = &[
    "level",
    "lvl",
    "msg",
    "message",
    "time",
    "timestamp",
    "ts",
    "target",
    "logger",
    "module",
];

fn logfmt_core_text(value: &TraceValue) -> String {
    match value {
        TraceValue::String(value) => value.clone(),
        value => value.render_text(),
    }
}

fn parse_logfmt_fields(input: &str) -> Option<Vec<TraceValueField>> {
    let mut fields = Vec::new();
    let mut rest = input.trim_start();

    while !rest.is_empty() {
        let previous_len = rest.len();
        let (key, after_key) = take_logfmt_key(rest)?;
        let after_eq = after_key.strip_prefix('=')?;
        let (value, tail) = take_logfmt_value(after_eq)?;
        fields.push(TraceValueField::new(
            key,
            TraceValue::from_tracing_text(&value),
        ));
        rest = tail.trim_start();
        if !rest.is_empty() && rest.len() >= previous_len {
            return None;
        }
    }

    Some(fields)
}

fn take_logfmt_key(input: &str) -> Option<(&str, &str)> {
    let end = input.find('=')?;
    let key = &input[..end];
    if key.is_empty()
        || key
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '='))
    {
        return None;
    }
    Some((key, &input[end..]))
}

fn take_logfmt_value(input: &str) -> Option<(String, &str)> {
    if let Some(rest) = input.strip_prefix('"') {
        let mut value = String::new();
        let mut escaped = false;
        for (idx, ch) in rest.char_indices() {
            if escaped {
                value.push(match ch {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return Some((
                    serde_json::to_string(&value).unwrap_or_else(|_| "\"\"".to_string()),
                    &rest[idx + ch.len_utf8()..],
                ));
            } else {
                value.push(ch);
            }
        }
        return None;
    }

    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    Some((input[..end].to_string(), &input[end..]))
}

fn parse_tracing(raw: &str) -> Option<LogEntry> {
    let raw = raw.trim_end();
    let (first, rest) = take_token(raw)?;

    let (timestamp, level, rest) = if let Some(level) = parse_level(first) {
        (None, level, rest)
    } else {
        let (second, rest) = take_token(rest)?;
        (Some(first.to_string()), parse_level(second)?, rest)
    };

    let (target, spans, message) = split_tracing_target_message(rest);
    let mut values = span_value_sections(&spans);
    let (message_parts, message_values) = tracing_message_parts(&message);
    if !message_values.is_empty() {
        values.push(TraceValueSection::new("event", message_values));
    }
    let message = MessagePart::plain_text(&message_parts);

    Some(LogEntry {
        raw: raw.to_string(),
        timestamp,
        level,
        parsed: true,
        target,
        spans,
        values,
        message,
        message_parts,
        stream: Stream::Stdout,
    })
}

fn parse_bunyan(raw: &str, stream: Stream) -> Option<LogEntry> {
    let raw = raw.trim_end();
    let value: Value = serde_json::from_str(raw).ok()?;
    let Value::Object(fields) = value else {
        return None;
    };

    let level = parse_bunyan_level(fields.get("level")?)?;
    let message = fields.get("msg")?.as_str()?.to_string();
    let timestamp = fields
        .get("time")
        .and_then(Value::as_str)
        .map(str::to_string);
    let target = fields
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let message_parts = bunyan_message_parts(&message, &fields);
    let message = MessagePart::plain_text(&message_parts);

    Some(LogEntry {
        raw: raw.to_string(),
        timestamp,
        level,
        parsed: true,
        target,
        spans: Vec::new(),
        values: Vec::new(),
        message,
        message_parts,
        stream,
    })
}

fn parse_bunyan_level(value: &Value) -> Option<Level> {
    if let Some(level) = value.as_i64() {
        return match level {
            10 => Some(Level::Trace),
            20 => Some(Level::Debug),
            30 => Some(Level::Info),
            40 => Some(Level::Warn),
            50 | 60 => Some(Level::Error),
            _ => None,
        };
    }

    value.as_str().and_then(parse_level)
}

fn bunyan_message_parts(message: &str, fields: &Map<String, Value>) -> Vec<MessagePart> {
    let extras: Vec<_> = fields
        .iter()
        .filter(|(key, _)| !BUNYAN_CORE_FIELDS.contains(&key.as_str()))
        .collect();

    let mut parts = vec![MessagePart::text(message)];
    if extras.is_empty() {
        return parts;
    }

    parts.push(MessagePart::fields(
        extras
            .into_iter()
            .map(|(key, value)| {
                TraceValueField::new(key.clone(), TraceValue::from_json(value.clone()))
            })
            .collect(),
    ));
    parts
}

#[derive(Debug, Eq, PartialEq)]
struct TracingField {
    key: String,
    value: TraceValue,
}

fn tracing_message_parts(message: &str) -> (Vec<MessagePart>, Vec<TraceValueField>) {
    let Some((message, fields)) = split_tracing_message_fields(message) else {
        return (vec![MessagePart::text(message)], Vec::new());
    };

    let mut parts = Vec::new();
    if !message.is_empty() {
        parts.push(MessagePart::text(message));
    }
    let values = fields
        .into_iter()
        .map(|field| TraceValueField::new(field.key, field.value))
        .collect::<Vec<_>>();
    parts.push(MessagePart::fields(values.clone()));
    (parts, values)
}

fn span_value_sections(spans: &[String]) -> Vec<TraceValueSection> {
    spans
        .iter()
        .filter_map(|span| {
            let open = span.find('{')?;
            if !span.ends_with('}') {
                return None;
            }

            let fields: Vec<_> = split_top_level(&span[open + 1..span.len() - 1], ',')
                .into_iter()
                .flat_map(span_segment_value_fields)
                .collect();
            if fields.is_empty() {
                return None;
            }

            Some(TraceValueSection::new(
                format!("scope: {}", span[..open].trim()),
                fields,
            ))
        })
        .collect()
}

fn span_segment_value_fields(segment: &str) -> Vec<TraceValueField> {
    if let Some(fields) = parse_tracing_field_sequence(segment) {
        return fields
            .into_iter()
            .map(|field| TraceValueField::new(field.key, field.value))
            .collect();
    }

    parse_span_value_field(segment).into_iter().collect()
}

fn parse_span_value_field(field: &str) -> Option<TraceValueField> {
    let (separator, _) = field
        .char_indices()
        .find(|(_, ch)| matches!(ch, '=' | ':'))?;
    let key = field[..separator].trim();
    let value = field[separator + 1..].trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }

    Some(TraceValueField::new(
        key,
        TraceValue::from_tracing_text(value),
    ))
}

fn split_top_level(value: &str, separator: char) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_quote = false;
    let mut escaped = false;

    for (idx, ch) in value.char_indices() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quote = false;
            }
            continue;
        }

        match ch {
            '"' => in_quote = true,
            '{' | '[' | '(' => depth = depth.saturating_add(1),
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            ch if ch == separator && depth == 0 => {
                fields.push(value[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }

    fields.push(value[start..].trim());
    fields
}

fn split_tracing_message_fields(message: &str) -> Option<(String, Vec<TracingField>)> {
    TracingFieldParser::new().split_message_fields(message)
}

fn parse_tracing_field_sequence(value: &str) -> Option<Vec<TracingField>> {
    TracingFieldParser::new().parse_sequence(value)
}

struct TracingFieldParser {
    budget: usize,
}

impl TracingFieldParser {
    fn new() -> Self {
        Self {
            budget: MAX_TRACING_FIELD_PARSE_STEPS,
        }
    }

    fn exhausted(&self) -> bool {
        self.budget == 0
    }

    fn consume_step(&mut self) -> Option<()> {
        self.budget = self.budget.checked_sub(1)?;
        Some(())
    }

    fn split_message_fields(&mut self, message: &str) -> Option<(String, Vec<TracingField>)> {
        for (idx, _) in message.char_indices() {
            if self.exhausted() {
                return None;
            }

            if idx > 0 && !message[..idx].ends_with(char::is_whitespace) {
                continue;
            }

            let candidate = &message[idx..];
            if let Some(fields) = self.parse_sequence(candidate) {
                return Some((message[..idx].trim_end().to_string(), fields));
            }
        }

        None
    }

    fn parse_sequence(&mut self, value: &str) -> Option<Vec<TracingField>> {
        self.consume_step()?;
        let value = value.trim_start();
        let (key, rest) = take_tracing_field_key(value)?;

        for end in self.value_end_candidates(rest) {
            self.consume_step()?;
            let field_value = rest[..end].trim_end();
            if field_value.is_empty() {
                continue;
            }

            let tail = rest[end..].trim_start();
            let field = TracingField {
                key: key.to_string(),
                value: TraceValue::from_tracing_text(field_value),
            };
            if tail.is_empty() {
                if unquoted_value_has_top_level_whitespace(field_value) {
                    continue;
                }
                return Some(vec![field]);
            }

            if let Some(mut fields) = self.parse_sequence(tail) {
                fields.insert(0, field);
                return Some(fields);
            }
        }

        None
    }

    fn value_end_candidates(&mut self, value: &str) -> Vec<usize> {
        if value.starts_with('"') {
            return quoted_value_end(value)
                .map(|end| vec![end])
                .unwrap_or_default();
        }

        let mut ends = Vec::new();
        let mut depth = 0usize;
        let mut in_quote = false;
        let mut escaped = false;

        for (idx, ch) in value.char_indices() {
            if in_quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_quote = false;
                }
                continue;
            }

            match ch {
                '"' => in_quote = true,
                '{' | '[' | '(' => depth = depth.saturating_add(1),
                '}' | ']' | ')' => depth = depth.saturating_sub(1),
                ch if ch.is_whitespace() && depth == 0 => {
                    if self.exhausted() {
                        return ends;
                    }
                    self.budget -= 1;
                    ends.push(idx);
                }
                _ => {}
            }
        }

        ends.push(value.len());
        ends
    }
}

fn unquoted_value_has_top_level_whitespace(value: &str) -> bool {
    if value.starts_with('"') {
        return false;
    }

    let mut depth = 0usize;
    let mut in_quote = false;
    let mut escaped = false;

    for ch in value.chars() {
        if in_quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quote = false;
            }
            continue;
        }

        match ch {
            '"' => in_quote = true,
            '{' | '[' | '(' => depth = depth.saturating_add(1),
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            ch if ch.is_whitespace() && depth == 0 => return true,
            _ => {}
        }
    }

    false
}

fn take_tracing_field_key(value: &str) -> Option<(&str, &str)> {
    let mut chars = value.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }

    for (idx, ch) in chars {
        if ch == '=' {
            return Some((&value[..idx], &value[idx + ch.len_utf8()..]));
        }
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')) {
            return None;
        }
    }

    None
}

fn quoted_value_end(value: &str) -> Option<usize> {
    let mut escaped = false;

    for (idx, ch) in value.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(idx + ch.len_utf8());
        }
    }

    None
}

fn take_token(value: &str) -> Option<(&str, &str)> {
    let value = value.trim_start();
    let end = value.find(char::is_whitespace)?;
    let token = &value[..end];
    let rest = value[end..].trim_start();
    Some((token, rest))
}

fn split_tracing_target_message(rest: &str) -> (Option<String>, Vec<String>, String) {
    let (mut spans, rest) = extract_leading_spans(rest);

    if let Some(idx) = find_target_separator(rest) {
        let target = rest[..idx].trim().to_string();
        let (more_spans, message) = extract_leading_spans(rest[idx + 1..].trim_start());
        spans.extend(more_spans);
        return (non_empty(target), spans, message.to_string());
    }

    (None, spans, rest.to_string())
}

fn find_target_separator(rest: &str) -> Option<usize> {
    let mut depth = 0usize;

    for (idx, ch) in rest.char_indices() {
        match ch {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 && starts_with_whitespace(&rest[idx + ch.len_utf8()..]) => {
                return Some(idx);
            }
            _ => {}
        }
    }

    None
}

fn extract_leading_spans(mut message: &str) -> (Vec<String>, &str) {
    let mut spans = Vec::new();

    while let Some(idx) = find_leading_span_separator(message) {
        let candidate = message[..idx].trim();
        let rest = message[idx + 1..].trim_start();
        if !looks_like_span(candidate, rest, !spans.is_empty()) {
            break;
        }
        spans.push(candidate.to_string());
        message = rest;
    }

    (spans, message)
}

fn find_leading_span_separator(message: &str) -> Option<usize> {
    let mut depth = 0usize;

    for (idx, ch) in message.char_indices() {
        match ch {
            '{' => depth = depth.saturating_add(1),
            '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 && !is_part_of_path_separator(message, idx) => return Some(idx),
            _ => {}
        }
    }

    None
}

fn is_part_of_path_separator(value: &str, idx: usize) -> bool {
    value[..idx].ends_with(':') || value[idx + 1..].starts_with(':')
}

fn looks_like_span(candidate: &str, rest: &str, has_span_prefix: bool) -> bool {
    if candidate.contains("::") || candidate.is_empty() {
        return false;
    }

    if let Some(open) = candidate.find('{') {
        return candidate.ends_with('}') && is_span_name(&candidate[..open]);
    }

    is_span_name(candidate)
        && if has_span_prefix {
            starts_with_span_fragment(rest)
        } else {
            starts_with_bare_span_fragment(rest)
        }
}

fn starts_with_span_fragment(rest: &str) -> bool {
    let Some(idx) = find_leading_span_separator(rest) else {
        return false;
    };
    let candidate = rest[..idx].trim();
    if candidate.contains("::") || candidate.is_empty() {
        return false;
    }
    match candidate.find('{') {
        Some(open) => candidate.ends_with('}') && is_span_name(&candidate[..open]),
        None => is_span_name(candidate),
    }
}

fn starts_with_bare_span_fragment(rest: &str) -> bool {
    let Some(idx) = find_leading_span_separator(rest) else {
        return false;
    };
    let candidate = rest[..idx].trim();
    !candidate.contains("::") && !candidate.contains('{') && is_span_name(candidate)
}

fn is_span_name(candidate: &str) -> bool {
    candidate
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn starts_with_whitespace(value: &str) -> bool {
    value.chars().next().is_some_and(char::is_whitespace)
}

pub(crate) fn parse_level(value: &str) -> Option<Level> {
    match value
        .trim_matches(|ch: char| !ch.is_ascii_alphabetic())
        .to_ascii_uppercase()
        .as_str()
    {
        "TRACE" => Some(Level::Trace),
        "DEBUG" => Some(Level::Debug),
        "INFO" => Some(Level::Info),
        "WARN" | "WARNING" => Some(Level::Warn),
        "ERROR" => Some(Level::Error),
        _ => None,
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn parses_env_logger_default_shape() {
        let entry =
            parse_env_logger("[2026-06-15T12:01:02Z INFO  my_crate::worker] finished job 42")
                .expect("entry");

        assert_eq!(entry.level, Level::Info);
        assert!(entry.parsed);
        assert_eq!(entry.timestamp.as_deref(), Some("2026-06-15T12:01:02Z"));
        assert_eq!(entry.target.as_deref(), Some("my_crate::worker"));
        assert_eq!(entry.message, "finished job 42");
    }

    #[test]
    fn parses_logfmt_default_shape() {
        let entry = parse_logfmt(
            r#"time=2026-06-15T12:01:02Z level=info target=my_crate::worker msg="loaded user" user=alice count=7 ok=true"#,
            Stream::Stdout,
        )
        .expect("entry");

        assert_eq!(entry.level, Level::Info);
        assert!(entry.parsed);
        assert_eq!(entry.timestamp.as_deref(), Some("2026-06-15T12:01:02Z"));
        assert_eq!(entry.target.as_deref(), Some("my_crate::worker"));
        assert_eq!(entry.message, r#"loaded user (user=alice count=7 ok=true)"#);
        assert_eq!(
            entry.message_parts,
            vec![
                MessagePart::text("loaded user"),
                MessagePart::fields(vec![
                    TraceValueField::new("user", TraceValue::Other("alice".to_string())),
                    TraceValueField::new("count", TraceValue::Number("7".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                ]),
            ]
        );
        assert_eq!(
            entry.values,
            vec![TraceValueSection::new(
                "event",
                vec![
                    TraceValueField::new("user", TraceValue::Other("alice".to_string())),
                    TraceValueField::new("count", TraceValue::Number("7".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                ]
            )]
        );
    }

    #[test]
    fn parses_logfmt_quoted_escapes() {
        let entry = parse_logfmt(
            r#"level=warn msg="retry \"soon\"" path="/api widgets""#,
            Stream::Stderr,
        )
        .expect("entry");

        assert_eq!(entry.level, Level::Warn);
        assert_eq!(entry.stream, Stream::Stderr);
        assert_eq!(entry.message, r#"retry "soon" (path="/api widgets")"#);
    }

    #[test]
    fn parses_logfmt_empty_values_without_spinning() {
        let entry =
            parse_logfmt(r#"level=info msg= user_id=42 empty="#, Stream::Stdout).expect("entry");

        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.message, r#"(user_id=42 empty=)"#);
        assert_eq!(
            entry.values,
            vec![TraceValueSection::new(
                "event",
                vec![
                    TraceValueField::new("user_id", TraceValue::Number("42".to_string())),
                    TraceValueField::new("empty", TraceValue::Other(String::new())),
                ]
            )]
        );
    }

    #[test]
    fn invalid_logfmt_falls_back_to_unparsed_entry() {
        let entry = parse_log_line(
            LogFormat::Logfmt,
            Stream::Stdout,
            "not a logfmt line".to_string(),
        );

        assert!(!entry.parsed);
        assert_eq!(entry.message, "not a logfmt line");
    }

    #[test]
    fn auto_detects_logfmt() {
        let entry = parse_log_line(
            LogFormat::Auto,
            Stream::Stdout,
            r#"level=info msg="hello" count=2"#.to_string(),
        );

        assert!(entry.parsed);
        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.message, "hello (count=2)");
    }

    #[test]
    fn parses_tracing_default_shape() {
        let entry =
            parse_tracing("2026-06-15T12:01:02.123456Z  WARN my_crate::worker: retrying request")
                .expect("entry");

        assert_eq!(entry.level, Level::Warn);
        assert_eq!(
            entry.timestamp.as_deref(),
            Some("2026-06-15T12:01:02.123456Z")
        );
        assert_eq!(entry.target.as_deref(), Some("my_crate::worker"));
        assert_eq!(entry.message, "retrying request");
    }

    #[test]
    fn parses_tracing_line_at_default_max_line_bytes() {
        const DEFAULT_MAX_LINE_BYTES: usize = 65_536;

        let prefix = "2026-06-15T12:01:02Z INFO svc: ";
        let message = "x".repeat(DEFAULT_MAX_LINE_BYTES - prefix.len());
        let line = format!("{prefix}{message}");

        let entry = parse_log_line(LogFormat::Tracing, Stream::Stdout, line);

        assert_eq!(entry.raw.len(), DEFAULT_MAX_LINE_BYTES);
        assert!(entry.parsed);
        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.target.as_deref(), Some("svc"));
        assert_eq!(entry.message, message);
    }

    #[test]
    fn tracing_field_parser_handles_default_sized_malformed_field_run() {
        const DEFAULT_MAX_LINE_BYTES: usize = 65_536;

        let prefix = "2026-06-15T12:01:02Z INFO svc: key=";
        let mut message = "x ".repeat((DEFAULT_MAX_LINE_BYTES - prefix.len()) / 2);
        message.push_str(&"x".repeat(DEFAULT_MAX_LINE_BYTES - prefix.len() - message.len()));
        let line = format!("{prefix}{message}");

        let entry = parse_log_line(LogFormat::Tracing, Stream::Stdout, line);

        assert_eq!(entry.raw.len(), DEFAULT_MAX_LINE_BYTES);
        assert!(entry.parsed);
        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.target.as_deref(), Some("svc"));
        assert_eq!(entry.message, format!("key={message}"));
        assert!(entry.values.is_empty());
    }

    #[test]
    fn tracing_field_parser_handles_default_sized_truncated_struct_list() {
        const DEFAULT_MAX_LINE_BYTES: usize = 65_536;

        let prefix = "2026-06-19T07:13:35Z TRACE svc: result=[";
        let item = r#"RankedSbom { matched_sbom_id: 019bbe7c-c5b7-75d3-b1ae-135527636ed3, matched_name: "requests", top_ancestor_sbom: 00000000-0000-0000-0000-000000000000, cpe_id: 0bdc06dc-647c-57ef-8060-5d824fb5a656, sbom_date: 2025-12-15T10:41:21+00:00, rank: Some(107) }, "#;
        let mut message = item.repeat((DEFAULT_MAX_LINE_BYTES - prefix.len()) / item.len());
        message.push_str(&item[..DEFAULT_MAX_LINE_BYTES - prefix.len() - message.len()]);
        let line = format!("{prefix}{message}");

        let entry = parse_log_line(LogFormat::Tracing, Stream::Stdout, line);

        assert_eq!(entry.raw.len(), DEFAULT_MAX_LINE_BYTES);
        assert!(entry.parsed);
        assert_eq!(entry.level, Level::Trace);
        assert_eq!(entry.target.as_deref(), Some("svc"));
        assert_eq!(entry.message, format!("(result=[{message})"));
        assert_eq!(entry.values.len(), 1);
    }

    #[test]
    fn extracts_tracing_span_hierarchy_before_message() {
        let entry = parse_tracing(
            "2026-06-15T12:01:02Z  INFO svc: request{id=7}: db{query=\"select:1\"}: loaded user",
        )
        .expect("entry");

        assert_eq!(entry.target.as_deref(), Some("svc"));
        assert_eq!(
            entry.spans,
            vec![
                "request{id=7}".to_string(),
                "db{query=\"select:1\"}".to_string()
            ]
        );
        assert_eq!(entry.message, "loaded user");
    }

    #[test]
    fn tracing_message_keeps_url_and_port_out_of_target() {
        let entry = parse_tracing(
            "2026-06-15T15:10:27.558965Z INFO  trustify_infrastructure::infra:    http://[::1]: 9010",
        )
        .expect("entry");

        assert_eq!(
            entry.timestamp.as_deref(),
            Some("2026-06-15T15:10:27.558965Z")
        );
        assert_eq!(entry.level, Level::Info);
        assert_eq!(
            entry.target.as_deref(),
            Some("trustify_infrastructure::infra")
        );
        assert!(entry.spans.is_empty());
        assert_eq!(entry.message, "http://[::1]: 9010");
    }

    #[test]
    fn extracts_tracing_spans_before_target() {
        let entry = parse_tracing(
            "2026-06-15T15:23:12.684277Z DEBUG retrieve_latest{query=Query { q: \"purl=pkg:maven/maven-xml-impl@4.0.0-alpha-5\", sort: \"\" } options=QueryOptions { ancestors: 0, descendants: 0, relationships: {} } paginated=Paginated { offset: 0, limit: 25 }}:load_latest_graphs_query{query=Query(Query { q: \"purl=pkg:maven/maven-xml-impl@4.0.0-alpha-5\", sort: \"\" })}: trustify_module_analysis::service::load: SBOM IDs to evaluate: 76",
        )
        .expect("entry");

        assert_eq!(entry.level, Level::Debug);
        assert_eq!(
            entry.target.as_deref(),
            Some("trustify_module_analysis::service::load")
        );
        assert_eq!(entry.spans.len(), 2);
        assert!(entry.spans[0].starts_with("retrieve_latest{query=Query"));
        assert!(entry.spans[1].starts_with("load_latest_graphs_query{query=Query"));
        assert_eq!(entry.message, "SBOM IDs to evaluate: 76");
    }

    #[test]
    fn extracts_bare_and_field_spans_before_target() {
        let entry = parse_tracing(
            "2026-06-15T15:35:27.706127Z TRACE load_graphs:load_graphs_inner:load_graph{distinct_sbom_id=019b9370-0a9d-7231-825b-3f6f3b80555a}:perform_load_graph{distinct_sbom_id=019b9370-0a9d-7231-825b-3f6f3b80555a}: retrieve_latest{query=Query { q: \"purl=pkg:maven/maven-xml-impl@4.0.0-alpha-5\", sort: \"\" }}: load_latest_graphs_query{query=Query(Query { q: \"purl=pkg:maven/maven-xml-impl@4.0.0-alpha-5\", sort: \"\" })}: trustify_module_analysis::service::load: Inserting - id: pkg:maven/org.wildfly.security/wildfly-elytron-x500-cert-util@2.6.3.Final-redhat-00001?type=jar, index: NodeIndex(1390)",
        )
        .expect("entry");

        assert_eq!(
            entry.spans,
            vec![
                "load_graphs".to_string(),
                "load_graphs_inner".to_string(),
                "load_graph{distinct_sbom_id=019b9370-0a9d-7231-825b-3f6f3b80555a}".to_string(),
                "perform_load_graph{distinct_sbom_id=019b9370-0a9d-7231-825b-3f6f3b80555a}".to_string(),
                "retrieve_latest{query=Query { q: \"purl=pkg:maven/maven-xml-impl@4.0.0-alpha-5\", sort: \"\" }}".to_string(),
                "load_latest_graphs_query{query=Query(Query { q: \"purl=pkg:maven/maven-xml-impl@4.0.0-alpha-5\", sort: \"\" })}".to_string(),
            ]
        );
        assert_eq!(
            entry.target.as_deref(),
            Some("trustify_module_analysis::service::load")
        );
        assert_eq!(
            entry.message,
            "Inserting - id: pkg:maven/org.wildfly.security/wildfly-elytron-x500-cert-util@2.6.3.Final-redhat-00001?type=jar, index: NodeIndex(1390)"
        );
    }

    #[rstest]
    #[case(
        "2022-02-15T18:40:14.289898Z  INFO fmt: preparing to shave yaks number_of_yaks=3",
        Level::Info,
        &[],
        "fmt",
        "preparing to shave yaks (number_of_yaks=3)",
    )]
    #[case(
        "2022-02-15T18:40:14.289974Z  INFO shaving_yaks{yaks=3}: fmt::yak_shave: shaving yaks",
        Level::Info,
        &["shaving_yaks{yaks=3}"],
        "fmt::yak_shave",
        "shaving yaks",
    )]
    #[case(
        "2022-02-15T18:40:14.290011Z TRACE shaving_yaks{yaks=3}:shave{yak=1}: fmt::yak_shave: hello! I'm gonna shave a yak excitement=\"yay!\"",
        Level::Trace,
        &["shaving_yaks{yaks=3}", "shave{yak=1}"],
        "fmt::yak_shave",
        "hello! I'm gonna shave a yak (excitement=\"yay!\")",
    )]
    #[case(
        "2022-02-15T18:40:14.290157Z DEBUG shaving_yaks{yaks=3}: yak_events: yak=3 shaved=false",
        Level::Debug,
        &["shaving_yaks{yaks=3}"],
        "yak_events",
        "(yak=3 shaved=false)",
    )]
    #[case(
        "2022-02-15T18:40:14.290268Z ERROR shaving_yaks{yaks=3}: fmt::yak_shave: failed to shave yak yak=3 error=missing yak error.sources=[out of space, out of cash]",
        Level::Error,
        &["shaving_yaks{yaks=3}"],
        "fmt::yak_shave",
        "failed to shave yak (yak=3 error=missing yak error.sources=[out of space, out of cash])",
    )]
    fn parses_tracing_fmt_documented_examples(
        #[case] line: &str,
        #[case] level: Level,
        #[case] spans: &[&str],
        #[case] target: &str,
        #[case] message: &str,
    ) {
        let entry = parse_tracing(line).expect(line);

        assert_eq!(entry.level, level);
        assert_eq!(entry.spans, spans);
        assert_eq!(entry.target.as_deref(), Some(target));
        assert_eq!(entry.message, message);
    }

    #[test]
    fn parses_tracing_message_fields_as_structured_parts() {
        let entry = parse_tracing(
            "2026-06-15T12:01:02Z INFO svc: loaded user id=7 ok=true tag=\"admin\" error.sources=[out of space, out of cash]",
        )
        .expect("entry");

        assert_eq!(
            entry.message,
            r#"loaded user (id=7 ok=true tag="admin" error.sources=[out of space, out of cash])"#
        );
        assert_eq!(
            entry.message_parts,
            vec![
                MessagePart::text("loaded user"),
                MessagePart::fields(vec![
                    TraceValueField::new("id", TraceValue::Number("7".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                    TraceValueField::new("tag", TraceValue::String("admin".to_string())),
                    TraceValueField::new(
                        "error.sources",
                        TraceValue::Other("[out of space, out of cash]".to_string())
                    ),
                ]),
            ]
        );
        assert_eq!(
            entry.values,
            vec![TraceValueSection::new(
                "event",
                vec![
                    TraceValueField::new("id", TraceValue::Number("7".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                    TraceValueField::new("tag", TraceValue::String("admin".to_string())),
                    TraceValueField::new(
                        "error.sources",
                        TraceValue::Other("[out of space, out of cash]".to_string())
                    ),
                ]
            )]
        );
    }

    #[test]
    fn parses_tracing_span_fields_as_values() {
        let entry =
            parse_tracing("2026-06-15T12:01:02Z INFO request{id=7, ok=true}: svc: loaded user")
                .expect("entry");

        assert_eq!(
            entry.values,
            vec![TraceValueSection::new(
                "scope: request",
                vec![
                    TraceValueField::new("id", TraceValue::Number("7".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                ]
            )]
        );
    }

    #[test]
    fn parses_whitespace_separated_tracing_span_fields_as_separate_values() {
        let entry =
            parse_tracing("2026-06-15T12:01:02Z INFO request{id=7 ok=true}: svc: loaded user")
                .expect("entry");

        assert_eq!(
            entry.values,
            vec![TraceValueSection::new(
                "scope: request",
                vec![
                    TraceValueField::new("id", TraceValue::Number("7".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                ]
            )]
        );
    }

    #[test]
    fn tracing_field_parser_gives_up_on_long_malformed_field_runs() {
        let fields = (0..2_000)
            .map(|idx| format!("field{idx}="))
            .collect::<Vec<_>>()
            .join(" ");
        let line = format!("2026-06-15T12:01:02Z INFO svc: {fields}");
        let entry = parse_tracing(&line).expect("entry");

        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.message, fields);
        assert!(entry.values.is_empty());
    }

    #[test]
    fn tracing_field_parser_keeps_equals_in_message_text() {
        let entry = parse_tracing("2026-06-15T12:01:02Z INFO svc: user typed mode=debug yesterday")
            .expect("entry");

        assert_eq!(entry.message, "user typed mode=debug yesterday");
        assert_eq!(
            entry.message_parts,
            vec![MessagePart::text("user typed mode=debug yesterday")]
        );
    }

    #[test]
    fn parses_bunyan_default_shape() {
        let entry = parse_bunyan(
            r#"{"name":"myapp","hostname":"banana.local","pid":40161,"level":30,"msg":"hi","time":"2013-01-04T18:46:23.851Z","v":0}"#,
            Stream::Stdout,
        )
        .expect("entry");

        assert_eq!(entry.level, Level::Info);
        assert!(entry.parsed);
        assert_eq!(entry.timestamp.as_deref(), Some("2013-01-04T18:46:23.851Z"));
        assert_eq!(entry.target.as_deref(), Some("myapp"));
        assert_eq!(entry.message, "hi");
        assert_eq!(entry.stream, Stream::Stdout);
        assert_eq!(entry.message_parts, vec![MessagePart::text("hi")]);
    }

    #[test]
    fn parses_bunyan_extra_fields_as_structured_message_parts() {
        let entry = parse_bunyan(
            r#"{"name":"myapp","hostname":"banana.local","pid":40161,"level":40,"lang":"fr","ok":true,"count":7,"msg":"au revoir","time":"2013-01-04T18:46:23.853Z","v":0}"#,
            Stream::Stderr,
        )
        .expect("entry");

        assert_eq!(entry.level, Level::Warn);
        assert_eq!(entry.stream, Stream::Stderr);
        assert_eq!(entry.message, r#"au revoir (lang="fr" ok=true count=7)"#);
        assert_eq!(
            entry.message_parts,
            vec![
                MessagePart::text("au revoir"),
                MessagePart::fields(vec![
                    TraceValueField::new("lang", TraceValue::String("fr".to_string())),
                    TraceValueField::new("ok", TraceValue::Bool(true)),
                    TraceValueField::new("count", TraceValue::Number("7".to_string())),
                ]),
            ]
        );
    }

    #[test]
    fn parses_bunyan_nested_extra_fields_compactly() {
        let entry = parse_bunyan(
            r#"{"name":"myapp","level":30,"msg":"request","req":{"method":"GET","status":200},"tags":["api",null,false],"v":0}"#,
            Stream::Stdout,
        )
        .expect("entry");

        assert_eq!(
            entry.message,
            r#"request (req={"method":"GET","status":200} tags=["api",null,false])"#
        );
        assert_eq!(
            entry.message_parts,
            vec![
                MessagePart::text("request"),
                MessagePart::fields(vec![
                    TraceValueField::new(
                        "req",
                        TraceValue::Object(vec![
                            ("method".to_string(), TraceValue::String("GET".to_string())),
                            ("status".to_string(), TraceValue::Number("200".to_string())),
                        ])
                    ),
                    TraceValueField::new(
                        "tags",
                        TraceValue::Array(vec![
                            TraceValue::String("api".to_string()),
                            TraceValue::Null,
                            TraceValue::Bool(false),
                        ])
                    ),
                ]),
            ]
        );
    }

    #[rstest]
    #[case(10, Level::Trace)]
    #[case(20, Level::Debug)]
    #[case(30, Level::Info)]
    #[case(40, Level::Warn)]
    #[case(50, Level::Error)]
    #[case(60, Level::Error)]
    fn maps_bunyan_numeric_levels(#[case] bunyan_level: u8, #[case] level: Level) {
        let entry = parse_bunyan(
            &format!(r#"{{"level":{bunyan_level},"msg":"level test"}}"#),
            Stream::Stdout,
        )
        .expect("entry");

        assert_eq!(entry.level, level);
    }

    #[test]
    fn parses_bunyan_string_level() {
        let entry =
            parse_bunyan(r#"{"level":"warn","msg":"careful"}"#, Stream::Stdout).expect("entry");

        assert_eq!(entry.level, Level::Warn);
    }

    #[test]
    fn auto_detects_bunyan_before_text_formats() {
        let entry = parse_log_line(
            LogFormat::Auto,
            Stream::Stdout,
            r#"{"level":30,"msg":"INFO my_crate: hello"}"#.to_string(),
        );

        assert!(entry.parsed);
        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.message, "INFO my_crate: hello");
        assert!(entry.target.is_none());
    }

    #[test]
    fn invalid_bunyan_falls_back_to_unparsed_entry() {
        let entry = parse_log_line(
            LogFormat::Bunyan,
            Stream::Stdout,
            r#"{"level":30,"message":"missing msg"}"#.to_string(),
        );

        assert!(!entry.parsed);
        assert_eq!(entry.level, Level::Unknown);
        assert_eq!(entry.message, r#"{"level":30,"message":"missing msg"}"#);
    }

    #[test]
    fn plain_fallback_keeps_original_line() {
        let entry = parse_log_line(LogFormat::Auto, Stream::Stdout, "hello there".to_string());

        assert_eq!(entry.level, Level::Unknown);
        assert!(!entry.parsed);
        assert_eq!(entry.message, "hello there");
    }

    #[test]
    fn strips_ansi_sequences_before_rendering() {
        let entry = parse_log_line(
            LogFormat::Auto,
            Stream::Stdout,
            "\u{1b}[32mINFO\u{1b}[0m my_crate: \u{1b}[31mhello\u{1b}[0m".to_string(),
        );

        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.target.as_deref(), Some("my_crate"));
        assert_eq!(entry.message, "hello");
    }

    #[test]
    fn strips_ansi_sequences_before_bunyan_parsing() {
        let entry = parse_log_line(
            LogFormat::Bunyan,
            Stream::Stdout,
            "\u{1b}[32m{\"level\":30,\"msg\":\"hello\"}\u{1b}[0m".to_string(),
        );

        assert!(entry.parsed);
        assert_eq!(entry.level, Level::Info);
        assert_eq!(entry.message, "hello");
    }
}
