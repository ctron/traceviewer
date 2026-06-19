use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{self, Event},
    terminal,
};

use crate::{
    cli::{Cli, LogFormat},
    clipboard::copy_to_clipboard,
    model::{AppEvent, Level, LogEntry},
    parser::parse_log_line,
    process::{InputSource, RunningInput, spawn_input},
    terminal::TerminalGuard,
    ui::{KeyAction, ViewState, content_rows, draw, handle_key, selected_line_text},
};

const MAX_EVENTS_PER_TICK: usize = 1024;

pub(crate) fn run(cli: Cli) -> Result<()> {
    let source = if let Some(file) = &cli.file {
        InputSource::File(file)
    } else {
        InputSource::Command(&cli.command)
    };
    let input = spawn_input(source, cli.max_line_bytes)?;

    let terminal = TerminalGuard::enter()?;
    let result = event_loop(&terminal, &input, cli.format, cli.max_lines);
    terminal.leave()?;

    result
}

fn event_loop(
    terminal: &TerminalGuard,
    input: &RunningInput,
    format: LogFormat,
    max_lines: Option<NonZeroUsize>,
) -> Result<()> {
    let mut entries = VecDeque::new();
    let mut state = ViewState::new();
    let mut exit_status = None;
    let mut input_finished = false;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut dirty = true;

    loop {
        let page_size = content_rows(terminal::size()?.1, &state);

        for _ in 0..MAX_EVENTS_PER_TICK {
            let Ok(app_event) = input.events.try_recv() else {
                break;
            };
            match app_event {
                AppEvent::Line(stream, line) => {
                    let was_following_latest = state
                        .selected
                        .is_none_or(|selected| selected + 1 == entries.len());

                    if max_lines.is_some_and(|max_lines| entries.len() == max_lines.get()) {
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
                AppEvent::InputFinished => {
                    input_finished = true;
                    dirty = true;
                }
                AppEvent::ReaderFailed(stream, err) => {
                    let message = format!("{stream:?} reader failed: {err}");
                    entries.push_back(LogEntry {
                        raw: message.clone(),
                        timestamp: None,
                        level: Level::Error,
                        parsed: false,
                        target: Some("traceviewer".to_string()),
                        spans: Vec::new(),
                        values: Vec::new(),
                        message,
                        message_parts: Vec::new(),
                        stream,
                    });
                    dirty = true;
                }
            }
        }

        if dirty && last_draw.elapsed() >= Duration::from_millis(16) {
            let mut stdout = terminal.stdout()?;
            draw(&mut *stdout, &entries, &state, exit_status, input_finished)?;
            last_draw = Instant::now();
            dirty = false;
        }

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match handle_key(
                key,
                &entries,
                &mut state,
                exit_status.is_some() || input_finished,
                page_size,
            ) {
                KeyAction::Continue => {}
                KeyAction::CopySelected => {
                    if let Some(line) = selected_line_text(&entries, &state) {
                        copy_to_clipboard(&line)?;
                    }
                }
                KeyAction::Quit => {
                    if exit_status.is_none() && !input_finished {
                        input.terminate();
                    }
                    break;
                }
            }
            dirty = true;
        }
    }

    Ok(())
}
