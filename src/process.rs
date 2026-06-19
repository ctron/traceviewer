use std::{
    fs::File,
    io::{self, BufReader},
    num::NonZeroUsize,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use anyhow::{Context, Result, anyhow};

use crate::model::{AppEvent, Stream};

const EVENT_BUFFER_SIZE: usize = 4096;

pub(crate) struct RunningInput {
    pub(crate) events: Receiver<AppEvent>,
    child: Option<Arc<Mutex<Child>>>,
}

pub(crate) enum InputSource<'a> {
    Command(&'a [String]),
    File(&'a Path),
}

impl RunningInput {
    pub(crate) fn terminate(&self) {
        let Some(child) = &self.child else {
            return;
        };
        let Ok(mut child) = child.lock() else {
            return;
        };

        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }

        let _ = child.kill();
    }
}

pub(crate) fn spawn_input(
    source: InputSource<'_>,
    max_line_bytes: NonZeroUsize,
) -> Result<RunningInput> {
    match source {
        InputSource::Command(command) => spawn_command(command, max_line_bytes),
        InputSource::File(path) => spawn_file(path, max_line_bytes),
    }
}

fn spawn_command(command: &[String], max_line_bytes: NonZeroUsize) -> Result<RunningInput> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow!("missing command"))?;

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{program}`"))?;

    let stdout = child.stdout.take().context("failed to capture stdout")?;
    let stderr = child.stderr.take().context("failed to capture stderr")?;

    let (tx, rx) = mpsc::sync_channel(EVENT_BUFFER_SIZE);
    spawn_reader(Stream::Stdout, stdout, tx.clone(), max_line_bytes);
    spawn_reader(Stream::Stderr, stderr, tx.clone(), max_line_bytes);

    let child = Arc::new(Mutex::new(child));
    let waiter_child = Arc::clone(&child);
    thread::spawn(move || {
        let event = wait_for_child(waiter_child);
        let _ = tx.send(event);
    });

    Ok(RunningInput {
        events: rx,
        child: Some(child),
    })
}

fn spawn_file(path: &Path, max_line_bytes: NonZeroUsize) -> Result<RunningInput> {
    let file = File::open(path).with_context(|| format!("failed to open `{}`", path.display()))?;
    let (tx, rx) = mpsc::sync_channel(EVENT_BUFFER_SIZE);
    spawn_file_reader(file, tx, max_line_bytes);

    Ok(RunningInput {
        events: rx,
        child: None,
    })
}

fn wait_for_child(child: Arc<Mutex<Child>>) -> AppEvent {
    loop {
        let status = match child.lock() {
            Ok(mut child) => child.try_wait(),
            Err(err) => {
                return AppEvent::ReaderFailed(Stream::Stderr, format!("wait lock failed: {err}"));
            }
        };

        match status {
            Ok(Some(status)) => return AppEvent::ProcessExited(status),
            Ok(None) => thread::sleep(std::time::Duration::from_millis(50)),
            Err(err) => {
                return AppEvent::ReaderFailed(Stream::Stderr, format!("wait failed: {err}"));
            }
        }
    }
}

fn spawn_file_reader<R>(reader: R, tx: SyncSender<AppEvent>, max_line_bytes: NonZeroUsize)
where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        if let Err(err) =
            StreamReader::new(Stream::Stdout, reader, max_line_bytes, &tx).read_lines()
        {
            let _ = tx.send(AppEvent::ReaderFailed(Stream::Stdout, err.to_string()));
            return;
        }
        let _ = tx.send(AppEvent::InputFinished);
    });
}

fn spawn_reader<R>(
    stream: Stream,
    reader: R,
    tx: SyncSender<AppEvent>,
    max_line_bytes: NonZeroUsize,
) where
    R: io::Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        if let Err(err) = StreamReader::new(stream, reader, max_line_bytes, &tx).read_lines() {
            let _ = tx.send(AppEvent::ReaderFailed(stream, err.to_string()));
        }
    });
}

struct StreamReader<'a, R> {
    stream: Stream,
    reader: BufReader<R>,
    tx: &'a SyncSender<AppEvent>,
    line: LineBuffer,
}

impl<'a, R> StreamReader<'a, R>
where
    R: io::Read,
{
    fn new(
        stream: Stream,
        reader: BufReader<R>,
        max_line_bytes: NonZeroUsize,
        tx: &'a SyncSender<AppEvent>,
    ) -> Self {
        Self {
            stream,
            reader,
            tx,
            line: LineBuffer::new(max_line_bytes),
        }
    }

    fn read_lines(mut self) -> io::Result<()> {
        loop {
            let ends_line = {
                let buffer = io::BufRead::fill_buf(&mut self.reader)?;
                if buffer.is_empty() {
                    if !self.line.is_empty() {
                        let _ = self.send_line();
                    }
                    return Ok(());
                }

                if let Some(newline_idx) = buffer.iter().position(|byte| *byte == b'\n') {
                    self.line.append(&buffer[..newline_idx]);
                    io::BufRead::consume(&mut self.reader, newline_idx + 1);
                    true
                } else {
                    let consumed = buffer.len();
                    self.line.append(buffer);
                    io::BufRead::consume(&mut self.reader, consumed);
                    false
                }
            };

            if ends_line && !self.send_line() {
                return Ok(());
            }
        }
    }

    fn send_line(&mut self) -> bool {
        let line = self.line.take_string();
        self.tx.send(AppEvent::Line(self.stream, line)).is_ok()
    }
}

struct LineBuffer {
    bytes: Vec<u8>,
    max_bytes: NonZeroUsize,
    truncated: bool,
}

impl LineBuffer {
    fn new(max_bytes: NonZeroUsize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            truncated: false,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let remaining = self.max_bytes.get().saturating_sub(self.bytes.len());
        if remaining >= bytes.len() {
            self.bytes.extend_from_slice(bytes);
        } else {
            self.bytes.extend_from_slice(&bytes[..remaining]);
            self.truncated = true;
        }
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty() && !self.truncated
    }

    fn take_string(&mut self) -> String {
        let mut line = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            line.push_str(" ... [truncated]");
        }
        self.bytes.clear();
        self.truncated = false;
        line
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn read_test_lines(input: &[u8], max_line_bytes: usize) -> Vec<String> {
        let (tx, rx) = mpsc::sync_channel(EVENT_BUFFER_SIZE);
        StreamReader::new(
            Stream::Stdout,
            BufReader::new(Cursor::new(input.to_vec())),
            NonZeroUsize::new(max_line_bytes).expect("non-zero limit"),
            &tx,
        )
        .read_lines()
        .expect("read lines");
        drop(tx);

        rx.into_iter()
            .filter_map(|event| match event {
                AppEvent::Line(_, line) => Some(line),
                AppEvent::ReaderFailed(_, _)
                | AppEvent::InputFinished
                | AppEvent::ProcessExited(_) => None,
            })
            .collect()
    }

    #[test]
    fn file_reader_sends_lines_and_finished_event() {
        let (tx, rx) = mpsc::sync_channel(EVENT_BUFFER_SIZE);
        spawn_file_reader(
            Cursor::new(b"alpha\nbeta\n".to_vec()),
            tx,
            NonZeroUsize::new(10).expect("non-zero limit"),
        );

        let events: Vec<_> = rx.into_iter().collect();

        assert!(matches!(events.as_slice(), [
            AppEvent::Line(Stream::Stdout, alpha),
            AppEvent::Line(Stream::Stdout, beta),
            AppEvent::InputFinished,
        ] if alpha == "alpha" && beta == "beta"));
    }

    #[test]
    fn bounded_reader_keeps_short_lines() {
        assert_eq!(
            read_test_lines(b"alpha\nbeta\n", 10),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn bounded_reader_truncates_long_lines_and_recovers_at_newline() {
        assert_eq!(
            read_test_lines(b"abcdefghijkl\nnext\n", 5),
            vec!["abcde ... [truncated]".to_string(), "next".to_string()]
        );
    }

    #[test]
    fn bounded_reader_truncates_final_line_without_newline() {
        assert_eq!(
            read_test_lines(b"abcdefghijkl", 5),
            vec!["abcde ... [truncated]".to_string()]
        );
    }

    #[test]
    fn bounded_reader_uses_lossy_utf8_for_partial_codepoints() {
        assert_eq!(
            read_test_lines("aé\n".as_bytes(), 2),
            vec!["a\u{fffd} ... [truncated]".to_string()]
        );
    }
}
