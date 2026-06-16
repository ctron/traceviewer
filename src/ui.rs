use std::{cmp, collections::VecDeque, io::Write, process::ExitStatus};

use anyhow::Result;
use crossterm::{
    cursor::{Hide, MoveTo},
    event::{KeyCode, KeyEvent, KeyModifiers},
    queue,
    style::{
        Attribute, Color, Print, PrintStyledContent, ResetColor, SetAttribute, SetBackgroundColor,
        SetForegroundColor, StyledContent, Stylize, style,
    },
    terminal::{self, Clear, ClearType},
};

use crate::model::{Level, LogEntry, MessagePart, MessageStyle, Stream};

#[derive(Debug, Default)]
pub(crate) struct ViewState {
    pub(crate) x_offset: usize,
    pub(crate) first_visible: usize,
    pub(crate) selected: Option<usize>,
    pub(crate) help_visible: bool,
    pub(crate) show_spans: bool,
    pub(crate) focus_target: Option<String>,
}

impl ViewState {
    pub(crate) fn new() -> Self {
        Self {
            show_spans: true,
            ..Self::default()
        }
    }

    pub(crate) fn follow_latest(&mut self, entries: &VecDeque<LogEntry>, page_size: usize) {
        let visible = visible_indices(entries, self);
        self.selected = visible.last().copied();
        self.scroll_selected_into_view(entries, page_size);
    }

    pub(crate) fn remove_first_line(&mut self) {
        self.first_visible = self.first_visible.saturating_sub(1);
        self.selected = self.selected.map(|selected| selected.saturating_sub(1));
    }

    fn move_selected_to(&mut self, visible: &[usize], selected_visible: usize, page_size: usize) {
        self.selected = visible.get(selected_visible).copied();
        self.scroll_selected_into_visible_slice(visible, page_size);
    }

    fn move_selected_by(&mut self, visible: &[usize], delta: isize, page_size: usize) {
        let Some(selected_pos) = selected_visible_pos(visible, self.selected) else {
            return;
        };
        let selected_pos = if delta.is_negative() {
            selected_pos.saturating_sub(delta.unsigned_abs())
        } else {
            cmp::min(
                selected_pos.saturating_add(delta as usize),
                visible.len().saturating_sub(1),
            )
        };

        self.move_selected_to(visible, selected_pos, page_size);
    }

    fn scroll_selected_into_view(&mut self, entries: &VecDeque<LogEntry>, page_size: usize) {
        let visible = visible_indices(entries, self);
        self.scroll_selected_into_visible_slice(&visible, page_size);
    }

    fn scroll_selected_into_visible_slice(&mut self, visible: &[usize], page_size: usize) {
        let Some(selected) = self.selected else {
            self.first_visible = 0;
            return;
        };
        let Some(selected_pos) = visible.iter().position(|idx| *idx == selected) else {
            self.first_visible = visible.first().copied().unwrap_or(0);
            self.selected = visible.last().copied();
            return;
        };
        if visible.is_empty() {
            self.first_visible = 0;
            self.selected = None;
            return;
        }

        let page_size = cmp::max(1, page_size);
        let first_pos = visible
            .iter()
            .position(|idx| *idx == self.first_visible)
            .unwrap_or_else(|| {
                visible
                    .iter()
                    .position(|idx| *idx >= self.first_visible)
                    .unwrap_or_else(|| visible.len().saturating_sub(1))
            });
        let max_first_pos = visible.len().saturating_sub(page_size);
        let mut first_pos = cmp::min(first_pos, max_first_pos);

        if selected_pos < first_pos {
            first_pos = selected_pos;
        } else if selected_pos >= first_pos.saturating_add(page_size) {
            first_pos = selected_pos.saturating_add(1).saturating_sub(page_size);
        }
        self.first_visible = visible[first_pos];
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyAction {
    Continue,
    CopySelected,
    Quit,
}

pub(crate) fn handle_key(
    key: KeyEvent,
    entries: &VecDeque<LogEntry>,
    state: &mut ViewState,
    process_exited: bool,
    page_size: usize,
) -> KeyAction {
    let visible = visible_indices(entries, state);
    let page_step = cmp::max(1, page_size.saturating_sub(1));
    const HORIZONTAL_SCROLL_STEP: usize = 16;

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return KeyAction::Quit;
        }
        KeyCode::Char('?') => {
            state.help_visible = !state.help_visible;
            return KeyAction::Continue;
        }
        KeyCode::Esc | KeyCode::Char('q') if state.help_visible => {
            state.help_visible = false;
            return KeyAction::Continue;
        }
        _ if state.help_visible => return KeyAction::Continue,
        KeyCode::Char('s') => {
            state.show_spans = !state.show_spans;
            return KeyAction::Continue;
        }
        KeyCode::Char('f') => {
            state.focus_target = if state.focus_target.is_some() {
                None
            } else {
                state
                    .selected
                    .and_then(|selected| entries.get(selected))
                    .and_then(|entry| entry.target.clone())
            };
            state.scroll_selected_into_view(entries, page_size);
            return KeyAction::Continue;
        }
        KeyCode::Char('y') => return KeyAction::CopySelected,
        KeyCode::Char('q') | KeyCode::Esc => {
            if process_exited {
                return KeyAction::Quit;
            }
        }
        KeyCode::Left => {
            state.x_offset = state.x_offset.saturating_sub(HORIZONTAL_SCROLL_STEP);
        }
        KeyCode::Right => {
            state.x_offset = state.x_offset.saturating_add(HORIZONTAL_SCROLL_STEP);
        }
        KeyCode::Home => state.move_selected_to(&visible, 0, page_size),
        KeyCode::End => {
            state.move_selected_to(&visible, visible.len().saturating_sub(1), page_size)
        }
        KeyCode::Up => state.move_selected_by(&visible, -1, page_size),
        KeyCode::Down => state.move_selected_by(&visible, 1, page_size),
        KeyCode::PageUp => state.move_selected_by(&visible, -(page_step as isize), page_size),
        KeyCode::PageDown => state.move_selected_by(&visible, page_step as isize, page_size),
        _ => {}
    }

    KeyAction::Continue
}

pub(crate) fn draw(
    stdout: &mut impl Write,
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
    exit_status: Option<ExitStatus>,
) -> Result<()> {
    let (cols, rows) = terminal::size()?;
    let content_rows = rows.saturating_sub(1) as usize;
    if state.help_visible {
        queue!(stdout, Hide, Clear(ClearType::All))?;
        draw_help_page(stdout, cols, content_rows)?;
        let status = status_line(entries.len(), state, exit_status, cols as usize);
        queue!(
            stdout,
            MoveTo(0, rows.saturating_sub(1)),
            PrintStyledContent(status.reverse())
        )?;
        stdout.flush()?;
        return Ok(());
    }

    let scrollbar_width = usize::from(cols > 1 && content_rows > 0);
    let log_width = (cols as usize).saturating_sub(scrollbar_width);
    let visible = visible_indices(entries, state);
    let selected = state
        .selected
        .filter(|selected| visible.contains(selected))
        .or_else(|| visible.last().copied());
    let start_pos = visible
        .iter()
        .position(|idx| *idx == state.first_visible)
        .unwrap_or_else(|| {
            visible
                .iter()
                .position(|idx| *idx >= state.first_visible)
                .unwrap_or(0)
        });
    let end_pos = cmp::min(start_pos + content_rows, visible.len());

    queue!(stdout, Hide, Clear(ClearType::All))?;

    for (screen_row, idx) in visible[start_pos..end_pos].iter().copied().enumerate() {
        let Some(entry) = entries.get(idx) else {
            continue;
        };
        queue!(stdout, MoveTo(0, screen_row as u16))?;
        EntryRenderer::from(state).draw(
            stdout,
            entry,
            state.x_offset,
            log_width,
            Some(idx) == selected,
        )?;
    }

    if scrollbar_width > 0 {
        draw_scrollbar(
            stdout,
            entries,
            &visible,
            start_pos,
            end_pos,
            content_rows,
            cols.saturating_sub(1),
        )?;
    }

    let status = status_line(entries.len(), state, exit_status, cols as usize);
    queue!(
        stdout,
        MoveTo(0, rows.saturating_sub(1)),
        PrintStyledContent(status.reverse())
    )?;
    stdout.flush()?;
    Ok(())
}

fn draw_scrollbar(
    stdout: &mut impl Write,
    entries: &VecDeque<LogEntry>,
    visible_indices: &[usize],
    visible_start: usize,
    visible_end: usize,
    height: usize,
    column: u16,
) -> Result<()> {
    for row in 0..height {
        let slice = ScrollbarSlice::new(row, height, visible_indices.len());
        let in_view = slice.start < visible_end && slice.end > visible_start;
        let color = scrollbar_slice_color(entries, visible_indices, slice.start, slice.end);
        let marker = if in_view { "#" } else { "|" };
        let mut styled = style(marker).with(color);
        if in_view {
            styled = styled.attribute(Attribute::Bold);
        }

        queue!(
            stdout,
            MoveTo(column, row as u16),
            PrintStyledContent(styled)
        )?;
    }

    Ok(())
}

fn draw_help_page(stdout: &mut impl Write, cols: u16, content_rows: usize) -> Result<()> {
    let lines = [
        "tv help",
        "",
        "Navigation",
        "  Up / Down       move cursor one line",
        "  PgUp / PgDown   move cursor one page",
        "  Home / Pos1     move cursor to first retained line",
        "  End             move cursor to last retained line",
        "  Left / Right    scroll horizontally",
        "",
        "Actions",
        "  f               focus selected target, or clear focus",
        "  s               toggle span information",
        "  y               copy selected line to clipboard",
        "  ?               toggle this help page",
        "  q / Esc         close help, or exit after the process ends",
        "  Ctrl-C          kill process and exit",
    ];

    for (row, line) in lines.iter().take(content_rows).enumerate() {
        let color = match *line {
            "tv help" | "Navigation" | "Actions" => Color::Cyan,
            _ => Color::White,
        };
        queue!(
            stdout,
            MoveTo(0, row as u16),
            PrintStyledContent(visible_slice(line, 0, cols as usize).with(color))
        )?;
    }

    Ok(())
}

pub(crate) fn selected_line_text(
    entries: &VecDeque<LogEntry>,
    state: &ViewState,
) -> Option<String> {
    entries
        .get(state.selected?)
        .map(|entry| EntryRenderer::from(state).plain_text(entry))
}

fn visible_indices(entries: &VecDeque<LogEntry>, state: &ViewState) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| entry_visible(entry, state).then_some(idx))
        .collect()
}

fn entry_visible(entry: &LogEntry, state: &ViewState) -> bool {
    state
        .focus_target
        .as_deref()
        .is_none_or(|target| entry.target.as_deref() == Some(target))
}

fn selected_visible_pos(visible: &[usize], selected: Option<usize>) -> Option<usize> {
    selected
        .and_then(|selected| visible.iter().position(|idx| *idx == selected))
        .or_else(|| visible.len().checked_sub(1))
}

#[derive(Clone, Copy, Debug)]
struct RenderOptions {
    show_spans: bool,
}

impl From<&ViewState> for RenderOptions {
    fn from(state: &ViewState) -> Self {
        Self {
            show_spans: state.show_spans,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Part {
    text: String,
    color: Color,
    bold: bool,
}

impl Part {
    fn new(text: impl Into<String>, color: Color, bold: bool) -> Self {
        Self {
            text: text.into(),
            color,
            bold,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct EntryRenderer {
    options: RenderOptions,
}

impl EntryRenderer {
    fn from(state: &ViewState) -> Self {
        Self {
            options: RenderOptions::from(state),
        }
    }

    fn draw(
        self,
        stdout: &mut impl Write,
        entry: &LogEntry,
        x_offset: usize,
        width: usize,
        selected: bool,
    ) -> Result<()> {
        let parts = self.parts(entry);
        let rendered = Self::plain_text_from_parts(&parts);
        let rendered_width = rendered.chars().count();
        if width == 0 {
            return Ok(());
        }

        let viewport = LineViewport::new(rendered_width, x_offset, width);
        let content_width = viewport.content_width;
        let visible = visible_slice(&rendered, x_offset, content_width);
        let mut cursor = 0usize;

        if viewport.show_left_marker {
            print_segment(stdout, "<".to_string(), Color::DarkGrey, true, selected)?;
        }

        for part in parts {
            let part_start = cursor;
            let part_end = cursor + part.text.chars().count();
            cursor = part_end;

            let overlap_start = cmp::max(part_start, x_offset);
            let overlap_end = cmp::min(part_end, x_offset.saturating_add(content_width));
            if overlap_start >= overlap_end {
                continue;
            }

            let local_start = overlap_start - part_start;
            let local_len = overlap_end - overlap_start;
            let segment: String = part
                .text
                .chars()
                .skip(local_start)
                .take(local_len)
                .collect();
            print_segment(stdout, segment, part.color, part.bold, selected)?;
        }

        let remaining = content_width.saturating_sub(visible.chars().count());
        if remaining > 0 {
            print_segment(stdout, " ".repeat(remaining), Color::White, false, selected)?;
        }

        if viewport.show_right_marker {
            print_segment(stdout, ">".to_string(), Color::DarkGrey, true, selected)?;
        }

        if selected {
            queue!(stdout, ResetColor)?;
        }
        Ok(())
    }

    fn plain_text(self, entry: &LogEntry) -> String {
        Self::plain_text_from_parts(&self.parts(entry))
    }

    fn plain_text_from_parts(parts: &[Part]) -> String {
        parts.iter().map(|part| part.text.as_str()).collect()
    }

    fn parts(self, entry: &LogEntry) -> Vec<Part> {
        let mut parts = Vec::new();
        parts.push(Part::new(
            format!("{} ", entry.stream.indicator()),
            stream_color(entry.stream),
            true,
        ));
        if let Some(timestamp) = &entry.timestamp {
            parts.push(Part::new(format!("{timestamp} "), Color::DarkGrey, false));
        }
        if entry.parsed {
            parts.push(Part::new(
                format!("{:<5} ", entry.level.label()),
                level_color(entry.level),
                true,
            ));
        }
        if let Some(target) = &entry.target {
            self.push_target_parts(&mut parts, target);
        }
        if self.options.show_spans {
            self.push_span_parts(&mut parts, &entry.spans);
        }
        self.push_message_parts(
            &mut parts,
            &entry.message,
            &entry.message_parts,
            message_color(entry),
        );
        parts
    }

    fn push_target_parts(self, parts: &mut Vec<Part>, target: &str) {
        let split_at = target
            .char_indices()
            .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx))
            .unwrap_or(target.len());
        let (module_path, suffix) = target.split_at(split_at);

        let mut modules = module_path.split("::").peekable();
        while let Some(module) = modules.next() {
            parts.push(Part::new(module, target_module_color(module), false));
            if modules.peek().is_some() {
                parts.push(Part::new("::", Color::DarkGrey, false));
            }
        }
        if !suffix.is_empty() {
            parts.push(Part::new(suffix, Color::DarkGrey, false));
        }
        parts.push(Part::new(": ", Color::DarkGrey, false));
    }

    fn push_span_parts(self, parts: &mut Vec<Part>, spans: &[String]) {
        for span in spans {
            self.push_span_part(parts, span);
            parts.push(Part::new(": ", Color::DarkGrey, false));
        }
    }

    fn push_span_part(self, parts: &mut Vec<Part>, span: &str) {
        if let Some(open) = span.find('{') {
            let (name, fields) = span.split_at(open);
            parts.push(Part::new(name, span_name_color(name), false));
            self.push_span_fields(parts, fields);
        } else {
            parts.push(Part::new(span, span_name_color(span), false));
        }
    }

    fn push_span_fields(self, parts: &mut Vec<Part>, fields: &str) {
        let mut current = String::new();
        let mut token = String::new();
        let mut chars = fields.chars().peekable();
        let mut in_string = false;
        let mut expecting_key = true;
        let mut expecting_value = false;

        while let Some(ch) = chars.next() {
            if in_string {
                token.push(ch);
                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                } else if ch == '"' {
                    parts.push(Part::new(std::mem::take(&mut token), string_color(), false));
                    in_string = false;
                    expecting_value = false;
                }
                continue;
            }

            match ch {
                '"' => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    token.push(ch);
                    in_string = true;
                }
                '=' | ':' => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    parts.push(Part::new(ch.to_string(), span_punctuation_color(), false));
                    expecting_key = false;
                    expecting_value = true;
                }
                '{' | '}' | '(' | ')' | '[' | ']' | ',' => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    parts.push(Part::new(ch.to_string(), span_punctuation_color(), false));
                    expecting_key = matches!(ch, '{' | ',' | '(');
                    expecting_value = false;
                }
                ch if ch.is_whitespace() => {
                    flush_span_token(parts, &mut token, expecting_key, expecting_value);
                    current.push(ch);
                    if !current.is_empty() {
                        parts.push(Part::new(std::mem::take(&mut current), Color::Reset, false));
                    }
                    expecting_key = !expecting_value;
                }
                _ => token.push(ch),
            }
        }

        if in_string {
            parts.push(Part::new(token, string_color(), false));
        } else {
            flush_span_token(parts, &mut token, expecting_key, expecting_value);
        }
    }
}

fn flush_span_token(
    parts: &mut Vec<Part>,
    token: &mut String,
    expecting_key: bool,
    expecting_value: bool,
) {
    if token.is_empty() {
        return;
    }

    let color = if expecting_key {
        span_key_color()
    } else if expecting_value {
        span_value_color(token)
    } else {
        Color::Reset
    };
    parts.push(Part::new(std::mem::take(token), color, false));
}

impl EntryRenderer {
    fn push_message_parts(
        self,
        parts: &mut Vec<Part>,
        message: &str,
        message_parts: &[MessagePart],
        base_color: Color,
    ) {
        if !message_parts.is_empty() {
            for part in message_parts {
                let (color, bold) = message_part_style(part.style, base_color);
                parts.push(Part::new(&part.text, color, bold));
            }
            return;
        }

        let mut current = String::new();
        let mut chars = message.chars().peekable();
        let mut in_string = false;

        while let Some(ch) = chars.next() {
            current.push(ch);

            if ch == '\\' && in_string {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
                continue;
            }

            if ch != '"' {
                continue;
            }

            if in_string {
                parts.push(Part::new(
                    std::mem::take(&mut current),
                    string_color(),
                    false,
                ));
                in_string = false;
            } else {
                if current.len() > ch.len_utf8() {
                    let quote = current.split_off(current.len() - ch.len_utf8());
                    parts.push(Part::new(std::mem::take(&mut current), base_color, false));
                    current = quote;
                }
                in_string = true;
            }
        }

        if !current.is_empty() {
            let color = if in_string {
                string_color()
            } else {
                base_color
            };
            parts.push(Part::new(current, color, false));
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct LineViewport {
    content_width: usize,
    show_left_marker: bool,
    show_right_marker: bool,
}

impl LineViewport {
    fn new(line_width: usize, x_offset: usize, terminal_width: usize) -> Self {
        let show_left_marker = x_offset > 0 && terminal_width > 1;
        let mut content_width = terminal_width.saturating_sub(usize::from(show_left_marker));
        let show_right_marker =
            line_width > x_offset.saturating_add(content_width) && content_width > 0;
        content_width = content_width.saturating_sub(usize::from(show_right_marker));

        Self {
            content_width,
            show_left_marker,
            show_right_marker,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ScrollbarSlice {
    start: usize,
    end: usize,
}

impl ScrollbarSlice {
    fn new(row: usize, height: usize, entries: usize) -> Self {
        if height == 0 || entries == 0 {
            return Self { start: 0, end: 0 };
        }

        let start = row.saturating_mul(entries) / height;
        let mut end = (row.saturating_add(1))
            .saturating_mul(entries)
            .div_ceil(height);
        end = cmp::min(cmp::max(end, start.saturating_add(1)), entries);

        Self { start, end }
    }
}

fn print_segment(
    stdout: &mut impl Write,
    text: String,
    color: Color,
    bold: bool,
    selected: bool,
) -> Result<()> {
    if selected {
        queue!(
            stdout,
            SetBackgroundColor(selected_background()),
            SetForegroundColor(selected_foreground(color))
        )?;
        if bold {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(stdout, Print(text))?;
        if bold {
            queue!(stdout, SetAttribute(Attribute::NormalIntensity))?;
        }
    } else {
        queue!(stdout, PrintStyledContent(apply_style(text, color, bold)))?;
    }

    Ok(())
}

fn apply_style(text: String, color: Color, bold: bool) -> StyledContent<String> {
    let mut styled = style(text).with(color);
    if bold {
        styled = styled.attribute(Attribute::Bold);
    }
    styled
}

fn selected_background() -> Color {
    Color::Rgb {
        r: 64,
        g: 64,
        b: 64,
    }
}

fn selected_foreground(color: Color) -> Color {
    match color {
        Color::Red => Color::DarkRed,
        Color::Yellow => Color::DarkYellow,
        Color::Green => Color::DarkGreen,
        Color::Blue => Color::DarkBlue,
        Color::Cyan => Color::DarkCyan,
        Color::White | Color::Grey => Color::Reset,
        other => other,
    }
}

fn status_line(
    entries: usize,
    state: &ViewState,
    exit_status: Option<ExitStatus>,
    width: usize,
) -> String {
    let selected = state.selected.map(|idx| idx + 1).unwrap_or(0);
    let follow = if state.selected.is_some_and(|idx| idx + 1 == entries) {
        " | auto-scroll"
    } else {
        ""
    };
    let process = match exit_status {
        Some(status) => format!("exited {status}"),
        None => "running".to_string(),
    };
    let focus = state
        .focus_target
        .as_deref()
        .map(|target| format!(" | focus {target}"))
        .unwrap_or_default();

    let status = format!(
        " {process} | line {selected}/{entries}{follow} | x={} | spans {}{focus} | ? help ",
        state.x_offset,
        if state.show_spans { "on" } else { "off" }
    );
    visible_slice(&format!("{status:<width$}"), 0, width)
}

fn level_color(level: Level) -> Color {
    match level {
        Level::Trace => Color::DarkGrey,
        Level::Debug => Color::Cyan,
        Level::Info => Color::Green,
        Level::Warn => Color::Yellow,
        Level::Error => Color::Red,
        Level::Unknown => Color::White,
    }
}

fn stream_color(stream: Stream) -> Color {
    match stream {
        Stream::Stdout => Color::DarkGrey,
        Stream::Stderr => Color::Yellow,
    }
}

fn string_color() -> Color {
    Color::Rgb {
        r: 206,
        g: 145,
        b: 120,
    }
}

fn message_part_style(style: MessageStyle, base_color: Color) -> (Color, bool) {
    match style {
        MessageStyle::Default => (base_color, false),
        MessageStyle::JsonArray | MessageStyle::JsonObject => (Color::Reset, true),
        MessageStyle::JsonBool | MessageStyle::JsonNumber => (Color::Reset, false),
        MessageStyle::JsonKey => (Color::Blue, true),
        MessageStyle::JsonNull => (Color::DarkGrey, false),
        MessageStyle::JsonPunctuation => (Color::DarkGrey, false),
        MessageStyle::JsonString => (Color::Green, false),
    }
}

fn span_name_color(span: &str) -> Color {
    span_palette_color(stable_hash(span) % SPAN_PALETTE_SIZE)
}

const SPAN_PALETTE_SIZE: usize = 64;

fn span_palette_color(index: usize) -> Color {
    let hue = (index as f32 * 360.0 / SPAN_PALETTE_SIZE as f32 + 18.0) % 360.0;
    let (r, g, b) = hsl_to_rgb(hue, 0.34, 0.62);

    Color::Rgb { r, g, b }
}

fn span_key_color() -> Color {
    Color::Rgb {
        r: 156,
        g: 220,
        b: 254,
    }
}

fn span_punctuation_color() -> Color {
    Color::Rgb {
        r: 150,
        g: 150,
        b: 150,
    }
}

fn span_value_color(value: &str) -> Color {
    if matches!(value, "true" | "false") {
        Color::Rgb {
            r: 86,
            g: 156,
            b: 214,
        }
    } else if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
        Color::Rgb {
            r: 181,
            g: 206,
            b: 168,
        }
    } else {
        Color::Reset
    }
}

fn target_module_color(module: &str) -> Color {
    target_palette_color(stable_hash(module) % TARGET_PALETTE_SIZE)
}

const TARGET_PALETTE_SIZE: usize = 128;

fn target_palette_color(index: usize) -> Color {
    let hue = (index as f32 * 360.0 / TARGET_PALETTE_SIZE as f32) % 360.0;
    let saturation = 0.48 + ((index / 32) as f32 * 0.09);
    let lightness = 0.58 + ((index / 16) % 2) as f32 * 0.10;
    let (r, g, b) = hsl_to_rgb(hue, saturation.min(0.78), lightness.min(0.72));

    Color::Rgb { r, g, b }
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_sector = hue / 60.0;
    let x = chroma * (1.0 - (hue_sector % 2.0 - 1.0).abs());

    let (r1, g1, b1) = match hue_sector as u8 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };

    let m = lightness - chroma / 2.0;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

fn stable_hash(value: &str) -> usize {
    value.bytes().fold(2_166_136_261usize, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ byte as usize
    })
}

fn message_color(entry: &LogEntry) -> Color {
    match (entry.level, entry.stream) {
        (Level::Error, _) => Color::Red,
        (Level::Warn, _) => Color::Yellow,
        (Level::Unknown, Stream::Stderr) => Color::Yellow,
        _ => Color::White,
    }
}

fn scrollbar_slice_color(
    entries: &VecDeque<LogEntry>,
    visible_indices: &[usize],
    start: usize,
    end: usize,
) -> Color {
    visible_indices
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .filter_map(|idx| entries.get(*idx))
        .map(|entry| entry.level)
        .max_by_key(|level| level.severity())
        .map(level_scrollbar_color)
        .unwrap_or(Color::DarkGrey)
}

fn level_scrollbar_color(level: Level) -> Color {
    match level {
        Level::Error => Color::Red,
        Level::Warn => Color::Yellow,
        Level::Info => Color::Green,
        Level::Debug => Color::Cyan,
        Level::Trace => Color::DarkGrey,
        Level::Unknown => Color::DarkGrey,
    }
}

fn visible_slice(input: &str, offset: usize, width: usize) -> String {
    input.chars().skip(offset).take(width).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(count: usize) -> VecDeque<LogEntry> {
        (0..count)
            .map(|idx| LogEntry {
                timestamp: None,
                level: Level::Info,
                parsed: true,
                target: None,
                spans: Vec::new(),
                message: format!("line {idx}"),
                message_parts: Vec::new(),
                stream: Stream::Stdout,
            })
            .collect()
    }

    fn entry_with_level(level: Level) -> LogEntry {
        LogEntry {
            timestamp: None,
            level,
            parsed: true,
            target: None,
            spans: Vec::new(),
            message: "line".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }
    }

    fn entry_with_target(target: Option<&str>, message: &str) -> LogEntry {
        LogEntry {
            timestamp: None,
            level: Level::Info,
            parsed: true,
            target: target.map(str::to_string),
            spans: Vec::new(),
            message: message.to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn renderer() -> EntryRenderer {
        EntryRenderer {
            options: RenderOptions { show_spans: true },
        }
    }

    #[test]
    fn home_and_end_move_to_first_and_last_lines() {
        let entries = entries(10);
        let mut state = ViewState::new();

        handle_key(key(KeyCode::Home), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(0));

        handle_key(key(KeyCode::End), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(9));
    }

    #[test]
    fn page_keys_move_by_visible_page() {
        let entries = entries(20);
        let mut state = ViewState::new();

        assert_eq!(
            handle_key(key(KeyCode::PageUp), &entries, &mut state, false, 6),
            KeyAction::Continue
        );
        assert_eq!(state.selected, Some(14));

        handle_key(key(KeyCode::PageDown), &entries, &mut state, false, 6);
        assert_eq!(state.selected, Some(19));
    }

    #[test]
    fn y_requests_copy_selected_line() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert_eq!(
            handle_key(key(KeyCode::Char('y')), &entries, &mut state, false, 5),
            KeyAction::CopySelected
        );
    }

    #[test]
    fn question_mark_toggles_help_page() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert_eq!(
            handle_key(key(KeyCode::Char('?')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(state.help_visible);

        assert_eq!(
            handle_key(key(KeyCode::Char('?')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(!state.help_visible);
    }

    #[test]
    fn s_toggles_span_information() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert!(state.show_spans);
        assert_eq!(
            handle_key(key(KeyCode::Char('s')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert!(!state.show_spans);

        handle_key(key(KeyCode::Char('s')), &entries, &mut state, false, 5);
        assert!(state.show_spans);
    }

    #[test]
    fn f_focuses_selected_target_and_clears_focus() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "one"),
            entry_with_target(Some("beta"), "two"),
            entry_with_target(Some("alpha"), "three"),
        ]);
        let mut state = ViewState {
            selected: Some(2),
            first_visible: 2,
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Char('f')), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert_eq!(state.focus_target.as_deref(), Some("alpha"));
        assert_eq!(visible_indices(&entries, &state), vec![0, 2]);
        assert_eq!(state.selected, Some(2));

        handle_key(key(KeyCode::Char('f')), &entries, &mut state, false, 5);
        assert_eq!(state.focus_target, None);
        assert_eq!(visible_indices(&entries, &state), vec![0, 1, 2]);
    }

    #[test]
    fn focused_navigation_skips_other_targets() {
        let entries = VecDeque::from([
            entry_with_target(Some("alpha"), "one"),
            entry_with_target(Some("beta"), "two"),
            entry_with_target(Some("alpha"), "three"),
            entry_with_target(Some("beta"), "four"),
        ]);
        let mut state = ViewState {
            selected: Some(0),
            focus_target: Some("alpha".to_string()),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(2));

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn f_without_selected_target_keeps_focus_clear() {
        let entries = VecDeque::from([entry_with_target(None, "plain")]);
        let mut state = ViewState {
            selected: Some(0),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Char('f')), &entries, &mut state, false, 5);
        assert_eq!(state.focus_target, None);
    }

    #[test]
    fn left_and_right_scroll_horizontally_by_sixteen_columns() {
        let entries = entries(1);
        let mut state = ViewState::new();

        assert_eq!(state.x_offset, 0);

        handle_key(key(KeyCode::Right), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 16);

        handle_key(key(KeyCode::Right), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 32);

        handle_key(key(KeyCode::Left), &entries, &mut state, false, 5);
        assert_eq!(state.x_offset, 16);
    }

    #[test]
    fn help_page_ignores_navigation_until_closed() {
        let entries = entries(10);
        let mut state = ViewState {
            help_visible: true,
            selected: Some(9),
            first_visible: 5,
            ..ViewState::new()
        };

        assert_eq!(
            handle_key(key(KeyCode::Up), &entries, &mut state, false, 5),
            KeyAction::Continue
        );
        assert_eq!(state.selected, Some(9));
        assert!(state.help_visible);

        handle_key(key(KeyCode::Esc), &entries, &mut state, false, 5);
        assert!(!state.help_visible);
    }

    #[test]
    fn selected_line_text_matches_rendered_plain_text() {
        let entries = VecDeque::from([LogEntry {
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("my_crate::worker".to_string()),
            spans: vec!["request{id=7}".to_string()],
            message: "loaded \"user\"".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }]);
        let state = ViewState {
            selected: Some(0),
            ..ViewState::new()
        };

        assert_eq!(
            selected_line_text(&entries, &state).as_deref(),
            Some("| 2026-06-15T12:01:02Z INFO  my_crate::worker: request{id=7}: loaded \"user\"")
        );
    }

    #[test]
    fn selected_line_text_omits_spans_when_hidden() {
        let entries = VecDeque::from([LogEntry {
            timestamp: Some("2026-06-15T12:01:02Z".to_string()),
            level: Level::Info,
            parsed: true,
            target: Some("my_crate::worker".to_string()),
            spans: vec!["request{id=7}".to_string()],
            message: "loaded \"user\"".to_string(),
            message_parts: Vec::new(),
            stream: Stream::Stdout,
        }]);
        let state = ViewState {
            selected: Some(0),
            show_spans: false,
            ..ViewState::new()
        };

        assert_eq!(
            selected_line_text(&entries, &state).as_deref(),
            Some("| 2026-06-15T12:01:02Z INFO  my_crate::worker: loaded \"user\"")
        );
    }

    #[test]
    fn cursor_moves_on_screen_before_scrolling_up() {
        let entries = entries(10);
        let mut state = ViewState::new();
        state.follow_latest(&entries, 5);

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(8));
        assert_eq!(state.first_visible, 5);

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(5));
        assert_eq!(state.first_visible, 5);

        handle_key(key(KeyCode::Up), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(4));
        assert_eq!(state.first_visible, 4);
    }

    #[test]
    fn cursor_moves_on_screen_before_scrolling_down() {
        let entries = entries(10);
        let mut state = ViewState {
            first_visible: 2,
            selected: Some(2),
            ..ViewState::new()
        };

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(3));
        assert_eq!(state.first_visible, 2);

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(6));
        assert_eq!(state.first_visible, 2);

        handle_key(key(KeyCode::Down), &entries, &mut state, false, 5);
        assert_eq!(state.selected, Some(7));
        assert_eq!(state.first_visible, 3);
    }

    #[test]
    fn line_viewport_marks_hidden_content_on_the_right() {
        assert_eq!(
            LineViewport::new(20, 0, 10),
            LineViewport {
                content_width: 9,
                show_left_marker: false,
                show_right_marker: true,
            }
        );
    }

    #[test]
    fn line_viewport_marks_hidden_content_on_both_sides() {
        assert_eq!(
            LineViewport::new(20, 5, 10),
            LineViewport {
                content_width: 8,
                show_left_marker: true,
                show_right_marker: true,
            }
        );
    }

    #[test]
    fn line_viewport_omits_markers_when_line_fits() {
        assert_eq!(
            LineViewport::new(8, 0, 10),
            LineViewport {
                content_width: 10,
                show_left_marker: false,
                show_right_marker: false,
            }
        );
    }

    #[test]
    fn scrollbar_slice_maps_row_to_entry_range() {
        assert_eq!(
            ScrollbarSlice::new(0, 5, 10),
            ScrollbarSlice { start: 0, end: 2 }
        );
        assert_eq!(
            ScrollbarSlice::new(2, 5, 10),
            ScrollbarSlice { start: 4, end: 6 }
        );
        assert_eq!(
            ScrollbarSlice::new(4, 5, 10),
            ScrollbarSlice { start: 8, end: 10 }
        );
    }

    #[test]
    fn scrollbar_slice_color_uses_highest_severity() {
        let entries = VecDeque::from([
            entry_with_level(Level::Info),
            entry_with_level(Level::Debug),
            entry_with_level(Level::Warn),
            entry_with_level(Level::Error),
        ]);

        let visible = [0, 1, 2, 3];

        assert_eq!(
            scrollbar_slice_color(&entries, &visible, 0, 2),
            Color::Green
        );
        assert_eq!(
            scrollbar_slice_color(&entries, &visible, 0, 3),
            Color::Yellow
        );
        assert_eq!(scrollbar_slice_color(&entries, &visible, 0, 4), Color::Red);
    }

    #[test]
    fn target_module_colors_are_stable_for_same_module() {
        assert_eq!(target_module_color("worker"), target_module_color("worker"));
    }

    #[test]
    fn target_palette_has_128_slots() {
        assert_eq!(TARGET_PALETTE_SIZE, 128);
        assert_ne!(target_palette_color(0), target_palette_color(127));
    }

    #[test]
    fn target_parts_split_rust_modules_and_keep_separators_neutral() {
        let mut parts = Vec::new();
        renderer().push_target_parts(&mut parts, "my_crate::worker::db");

        let text = EntryRenderer::plain_text_from_parts(&parts);
        assert_eq!(text, "my_crate::worker::db: ");
        assert_eq!(parts[1], Part::new("::", Color::DarkGrey, false));
        assert_eq!(parts[3], Part::new("::", Color::DarkGrey, false));
        assert_eq!(parts[5], Part::new(": ", Color::DarkGrey, false));
    }

    #[test]
    fn target_parts_do_not_split_modules_after_first_whitespace() {
        let mut parts = Vec::new();
        renderer().push_target_parts(&mut parts, "my_crate::worker span{path=other::module}");

        let text = EntryRenderer::plain_text_from_parts(&parts);
        assert_eq!(text, "my_crate::worker span{path=other::module}: ");
        assert_eq!(
            parts[3],
            Part::new(
                " span{path=other::module}".to_string(),
                Color::DarkGrey,
                false
            )
        );
    }

    #[test]
    fn message_parts_highlight_quoted_strings() {
        let mut parts = Vec::new();
        renderer().push_message_parts(
            &mut parts,
            "loaded \"user 42\" from cache",
            &[],
            Color::White,
        );

        assert_eq!(parts[0], Part::new("loaded ", Color::White, false));
        assert_eq!(parts[1], Part::new("\"user 42\"", string_color(), false));
        assert_eq!(parts[2], Part::new(" from cache", Color::White, false));
    }

    #[test]
    fn message_parts_keep_escaped_quotes_inside_string() {
        let mut parts = Vec::new();
        renderer().push_message_parts(
            &mut parts,
            "loaded \"user \\\"jonas\\\"\"",
            &[],
            Color::White,
        );

        assert_eq!(parts[0], Part::new("loaded ", Color::White, false));
        assert_eq!(
            parts[1],
            Part::new("\"user \\\"jonas\\\"\"", string_color(), false)
        );
    }

    #[test]
    fn structured_message_parts_use_jq_style_colors() {
        let message_parts = vec![
            MessagePart::new("au revoir", MessageStyle::Default),
            MessagePart::new(" (", MessageStyle::JsonPunctuation),
            MessagePart::new("lang", MessageStyle::JsonKey),
            MessagePart::new("=", MessageStyle::JsonPunctuation),
            MessagePart::new("\"fr\"", MessageStyle::JsonString),
            MessagePart::new(" ", MessageStyle::JsonPunctuation),
            MessagePart::new("ok", MessageStyle::JsonBool),
            MessagePart::new("=", MessageStyle::JsonPunctuation),
            MessagePart::new("true", MessageStyle::JsonBool),
            MessagePart::new(" ", MessageStyle::JsonPunctuation),
            MessagePart::new("count", MessageStyle::JsonNumber),
            MessagePart::new("=", MessageStyle::JsonPunctuation),
            MessagePart::new("7", MessageStyle::JsonNumber),
            MessagePart::new(" ", MessageStyle::JsonPunctuation),
            MessagePart::new("none", MessageStyle::JsonNull),
            MessagePart::new("=", MessageStyle::JsonPunctuation),
            MessagePart::new("null", MessageStyle::JsonNull),
            MessagePart::new(")", MessageStyle::JsonPunctuation),
        ];
        let mut parts = Vec::new();

        renderer().push_message_parts(&mut parts, "", &message_parts, Color::White);

        assert_eq!(
            EntryRenderer::plain_text_from_parts(&parts),
            r#"au revoir (lang="fr" ok=true count=7 none=null)"#
        );
        assert_eq!(parts[0], Part::new("au revoir", Color::White, false));
        assert_eq!(parts[2], Part::new("lang", Color::Blue, true));
        assert_eq!(parts[4], Part::new("\"fr\"", Color::Green, false));
        assert_eq!(parts[8], Part::new("true", Color::Reset, false));
        assert_eq!(parts[12], Part::new("7", Color::Reset, false));
        assert_eq!(parts[16], Part::new("null", Color::DarkGrey, false));
    }

    #[test]
    fn span_parts_use_span_specific_colors() {
        let spans = vec![
            "request{id=7}".to_string(),
            "db{query=\"select\"}".to_string(),
        ];
        let mut parts = Vec::new();

        renderer().push_span_parts(&mut parts, &spans);

        assert_eq!(
            parts[0],
            Part::new("request", span_name_color("request"), false)
        );
        assert_eq!(parts[1], Part::new("{", span_punctuation_color(), false));
        assert_eq!(parts[2], Part::new("id", span_key_color(), false));
        assert_eq!(parts[3], Part::new("=", span_punctuation_color(), false));
        assert_eq!(parts[4], Part::new("7", span_value_color("7"), false));
        assert_eq!(parts[6], Part::new(": ", Color::DarkGrey, false));
        assert_eq!(parts[7], Part::new("db", span_name_color("db"), false));
        assert_eq!(parts[10], Part::new("=", span_punctuation_color(), false));
        assert_eq!(parts[11], Part::new("\"select\"", string_color(), false));
    }

    #[test]
    fn span_parts_render_bare_spans_as_span_names() {
        let spans = vec!["load_graphs".to_string(), "load_graphs_inner".to_string()];
        let mut parts = Vec::new();

        renderer().push_span_parts(&mut parts, &spans);

        assert_eq!(
            parts[0],
            Part::new(
                "load_graphs".to_string(),
                span_name_color("load_graphs"),
                false
            )
        );
        assert_eq!(parts[1], Part::new(": ", Color::DarkGrey, false));
        assert_eq!(
            parts[2],
            Part::new(
                "load_graphs_inner".to_string(),
                span_name_color("load_graphs_inner"),
                false
            )
        );
    }

    #[test]
    fn span_palette_is_separate_from_target_palette() {
        assert_eq!(SPAN_PALETTE_SIZE, 64);
        assert_ne!(span_name_color("request"), target_module_color("request"));
    }
}
