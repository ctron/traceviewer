use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use crossterm::{
    event::{self, Event},
    terminal,
};

use crate::{
    cli::{Cli, LogFormat},
    clipboard::copy_to_clipboard,
    model::{AppEvent, Level, LogEntry},
    parser::parse_log_line,
    process::{RunningCommand, spawn_command},
    terminal::TerminalGuard,
    ui::{KeyAction, ViewState, draw, handle_key, selected_line_text},
};

pub(crate) fn run(cli: Cli) -> Result<()> {
    if cli.max_lines == Some(0) {
        bail!("--max-lines must be greater than zero");
    }

    let command = spawn_command(&cli.command)?;

    let terminal = TerminalGuard::enter()?;
    let result = event_loop(&terminal, &command, cli.format, cli.max_lines);
    terminal.leave()?;

    result
}

fn event_loop(
    terminal: &TerminalGuard,
    command: &RunningCommand,
    format: LogFormat,
    max_lines: Option<usize>,
) -> Result<()> {
    let mut entries = VecDeque::new();
    let mut state = ViewState::new();
    let mut exit_status = None;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut dirty = true;

    loop {
        let page_size = terminal::size()?.1.saturating_sub(1) as usize;

        while let Ok(app_event) = command.events.try_recv() {
            match app_event {
                AppEvent::Line(stream, line) => {
                    let was_following_latest = state
                        .selected
                        .is_none_or(|selected| selected + 1 == entries.len());

                    if max_lines.is_some_and(|max_lines| entries.len() == max_lines) {
                        entries.pop_front();
                        state.remove_first_line();
                    }
                    entries.push_back(parse_log_line(format, stream, line));
                    if was_following_latest {
                        state.follow_latest(&entries, page_size);
                    }
                    dirty = true;
                }
                AppEvent::ProcessExited(status) => {
                    exit_status = Some(status);
                    dirty = true;
                }
                AppEvent::ReaderFailed(stream, err) => {
                    let message = format!("{stream:?} reader failed: {err}");
                    entries.push_back(LogEntry {
                        timestamp: None,
                        level: Level::Error,
                        parsed: false,
                        target: Some("traceviewer".to_string()),
                        spans: Vec::new(),
                        message,
                        message_parts: Vec::new(),
                        stream,
                    });
                    dirty = true;
                }
            }
        }

        if dirty && last_draw.elapsed() >= Duration::from_millis(16) {
            let mut stdout = terminal.stdout();
            draw(&mut *stdout, &entries, &state, exit_status)?;
            last_draw = Instant::now();
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match handle_key(key, &entries, &mut state, exit_status.is_some(), page_size) {
                KeyAction::Continue => {}
                KeyAction::CopySelected => {
                    if let Some(line) = selected_line_text(&entries, &state) {
                        copy_to_clipboard(&line)?;
                    }
                }
                KeyAction::Quit => {
                    if exit_status.is_none() {
                        command.terminate();
                    }
                    break;
                }
            }
            dirty = true;
        }
    }

    Ok(())
}
