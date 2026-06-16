use crate::{
    cli::LogFormat,
    model::{Level, LogEntry, MessagePart, MessageStyle, Stream},
};
use serde_json::{Map, Value};

const BUNYAN_CORE_FIELDS: &[&str] = &["name", "hostname", "pid", "level", "msg", "time", "v"];

pub(crate) fn parse_log_line(format: LogFormat, stream: Stream, raw: String) -> LogEntry {
    let raw = strip_ansi_escape_sequences(&raw);
    let parsed = match format {
        LogFormat::Auto => parse_bunyan(&raw, stream)
            .or_else(|| parse_tracing(&raw))
            .or_else(|| parse_env_logger(&raw)),
        LogFormat::Bunyan => parse_bunyan(&raw, stream),
        LogFormat::Plain => None,
        LogFormat::EnvLogger => parse_env_logger(&raw),
        LogFormat::Tracing => parse_tracing(&raw),
    };

    parsed.unwrap_or_else(|| LogEntry {
        level: if stream == Stream::Stderr {
            Level::Warn
        } else {
            Level::Unknown
        },
        timestamp: None,
        target: None,
        spans: Vec::new(),
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
        timestamp,
        level,
        parsed: true,
        target,
        spans: Vec::new(),
        message: message.to_string(),
        message_parts: Vec::new(),
        stream: Stream::Stdout,
    })
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

    Some(LogEntry {
        timestamp,
        level,
        parsed: true,
        target,
        spans,
        message,
        message_parts: Vec::new(),
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
        timestamp,
        level,
        parsed: true,
        target,
        spans: Vec::new(),
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

    let mut parts = vec![MessagePart::new(message, MessageStyle::Default)];
    if extras.is_empty() {
        return parts;
    }

    parts.push(MessagePart::new(" (", MessageStyle::JsonPunctuation));
    for (idx, (key, value)) in extras.iter().enumerate() {
        if idx > 0 {
            parts.push(MessagePart::new(" ", MessageStyle::JsonPunctuation));
        }
        parts.push(MessagePart::new(*key, MessageStyle::JsonKey));
        parts.push(MessagePart::new("=", MessageStyle::JsonPunctuation));
        push_json_value_parts(&mut parts, value);
    }
    parts.push(MessagePart::new(")", MessageStyle::JsonPunctuation));
    parts
}

fn push_json_value_parts(parts: &mut Vec<MessagePart>, value: &Value) {
    match value {
        Value::Null => parts.push(MessagePart::new("null", MessageStyle::JsonNull)),
        Value::Bool(value) => {
            parts.push(MessagePart::new(value.to_string(), MessageStyle::JsonBool))
        }
        Value::Number(value) => parts.push(MessagePart::new(
            value.to_string(),
            MessageStyle::JsonNumber,
        )),
        Value::String(value) => parts.push(MessagePart::new(
            serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
            MessageStyle::JsonString,
        )),
        Value::Array(values) => {
            parts.push(MessagePart::new("[", MessageStyle::JsonArray));
            for (idx, value) in values.iter().enumerate() {
                if idx > 0 {
                    parts.push(MessagePart::new(",", MessageStyle::JsonPunctuation));
                }
                push_json_value_parts(parts, value);
            }
            parts.push(MessagePart::new("]", MessageStyle::JsonArray));
        }
        Value::Object(fields) => {
            parts.push(MessagePart::new("{", MessageStyle::JsonObject));
            for (idx, (key, value)) in fields.iter().enumerate() {
                if idx > 0 {
                    parts.push(MessagePart::new(",", MessageStyle::JsonPunctuation));
                }
                parts.push(MessagePart::new(
                    serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                    MessageStyle::JsonKey,
                ));
                parts.push(MessagePart::new(":", MessageStyle::JsonPunctuation));
                push_json_value_parts(parts, value);
            }
            parts.push(MessagePart::new("}", MessageStyle::JsonObject));
        }
    }
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
        "preparing to shave yaks number_of_yaks=3",
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
        "hello! I'm gonna shave a yak excitement=\"yay!\"",
    )]
    #[case(
        "2022-02-15T18:40:14.290157Z DEBUG shaving_yaks{yaks=3}: yak_events: yak=3 shaved=false",
        Level::Debug,
        &["shaving_yaks{yaks=3}"],
        "yak_events",
        "yak=3 shaved=false",
    )]
    #[case(
        "2022-02-15T18:40:14.290268Z ERROR shaving_yaks{yaks=3}: fmt::yak_shave: failed to shave yak yak=3 error=missing yak error.sources=[out of space, out of cash]",
        Level::Error,
        &["shaving_yaks{yaks=3}"],
        "fmt::yak_shave",
        "failed to shave yak yak=3 error=missing yak error.sources=[out of space, out of cash]",
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
        assert_eq!(
            entry.message_parts,
            vec![MessagePart::new("hi", MessageStyle::Default)]
        );
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
                MessagePart::new("au revoir", MessageStyle::Default),
                MessagePart::new(" (", MessageStyle::JsonPunctuation),
                MessagePart::new("lang", MessageStyle::JsonKey),
                MessagePart::new("=", MessageStyle::JsonPunctuation),
                MessagePart::new("\"fr\"", MessageStyle::JsonString),
                MessagePart::new(" ", MessageStyle::JsonPunctuation),
                MessagePart::new("ok", MessageStyle::JsonKey),
                MessagePart::new("=", MessageStyle::JsonPunctuation),
                MessagePart::new("true", MessageStyle::JsonBool),
                MessagePart::new(" ", MessageStyle::JsonPunctuation),
                MessagePart::new("count", MessageStyle::JsonKey),
                MessagePart::new("=", MessageStyle::JsonPunctuation),
                MessagePart::new("7", MessageStyle::JsonNumber),
                MessagePart::new(")", MessageStyle::JsonPunctuation),
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
        assert!(
            entry
                .message_parts
                .iter()
                .any(|part| part.text == "null" && part.style == MessageStyle::JsonNull)
        );
        assert!(
            entry
                .message_parts
                .iter()
                .any(|part| part.text == "[" && part.style == MessageStyle::JsonArray)
        );
        assert!(
            entry
                .message_parts
                .iter()
                .any(|part| part.text == "{" && part.style == MessageStyle::JsonObject)
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
