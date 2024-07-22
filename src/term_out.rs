use std::io::Write as _;
use std::sync::mpsc;
use std::time::Duration;

pub(crate) fn init(start: std::time::Instant, enable_progress: bool) -> Handle {
    let (sender, receiver) = mpsc::channel();

    let join_handle = std::thread::Builder::new()
        .name("term out".into())
        .spawn(move || thread_main(start, enable_progress, receiver))
        .expect("failed to spawn thread");

    Handle {
        join_handle,
        sender,
    }
}

const UPDATE_PERIOD: Duration = Duration::from_millis(50);

fn thread_main(
    start: std::time::Instant,
    enable_progress: bool,
    receiver: mpsc::Receiver<Command>,
) {
    let mut last_progress = None::<String>;
    let mut last_update = start;
    let mut needs_update = false;
    let mut stderr = std::io::stderr();

    loop {
        let mut timeout = None;
        if last_progress.is_some() {
            if needs_update {
                timeout = Some(UPDATE_PERIOD.saturating_sub(last_update.elapsed()));
            } else {
                timeout = Some(duration_to_next_second(start.elapsed()));
            }
        }

        let cmd = if let Some(timeout) = timeout {
            if timeout.is_zero() {
                Err(mpsc::RecvTimeoutError::Timeout)
            } else {
                receiver.recv_timeout(timeout)
            }
        } else {
            receiver.recv().map_err(|e| e.into())
        };

        match cmd {
            Ok(Command::Finish) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(ref last_progress) = last_progress {
                    if needs_update {
                        let progress_line = render_progress_line(start, last_progress);
                        handle_err(crossterm::queue!(
                            stderr,
                            crossterm::cursor::MoveToColumn(0),
                            crossterm::style::Print(progress_line),
                            crossterm::terminal::Clear(
                                crossterm::terminal::ClearType::UntilNewLine
                            ),
                        ));
                    }
                    handle_err(crossterm::queue!(
                        stderr,
                        crossterm::style::Print('\n'),
                        crossterm::cursor::MoveToColumn(0),
                    ));
                    handle_err(stderr.flush());
                }
                break;
            }
            Ok(Command::PrintRawLine(line)) => {
                if let Some(ref last_progress) = last_progress {
                    handle_err(crossterm::queue!(
                        stderr,
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine),
                        crossterm::cursor::MoveToColumn(0),
                    ));
                    handle_err(stderr.write_all(&line));
                    let progress_line = render_progress_line(start, last_progress);
                    handle_err(crossterm::queue!(
                        stderr,
                        crossterm::style::Print(progress_line),
                    ));
                } else {
                    handle_err(stderr.write_all(&line));
                }
                handle_err(stderr.flush());
            }
            Ok(Command::SetProgress(progress)) => {
                if enable_progress {
                    if last_update.elapsed() >= UPDATE_PERIOD {
                        let progress_line = render_progress_line(start, &progress);
                        handle_err(crossterm::queue!(
                            stderr,
                            crossterm::cursor::MoveToColumn(0),
                            crossterm::style::Print(progress_line),
                            crossterm::terminal::Clear(
                                crossterm::terminal::ClearType::UntilNewLine
                            ),
                        ));
                        handle_err(stderr.flush());
                        last_progress = Some(progress);
                        last_update = std::time::Instant::now();
                        needs_update = false;
                    } else {
                        last_progress = Some(progress);
                        needs_update = false;
                    }
                }
            }
            Ok(Command::FreezeProgress) => {
                if let Some(ref last_progress) = last_progress {
                    if needs_update {
                        let progress_line = render_progress_line(start, last_progress);
                        handle_err(crossterm::queue!(
                            stderr,
                            crossterm::cursor::MoveToColumn(0),
                            crossterm::style::Print(progress_line),
                            crossterm::terminal::Clear(
                                crossterm::terminal::ClearType::UntilNewLine
                            ),
                        ));
                    }
                    handle_err(crossterm::queue!(
                        stderr,
                        crossterm::style::Print('\n'),
                        crossterm::cursor::MoveToColumn(0),
                    ));
                    handle_err(stderr.flush());
                }
                last_progress = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(ref last_progress) = last_progress {
                    let progress_line = render_progress_line(start, last_progress);
                    handle_err(crossterm::queue!(
                        stderr,
                        crossterm::cursor::MoveToColumn(0),
                        crossterm::style::Print(progress_line),
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::UntilNewLine),
                    ));
                    handle_err(stderr.flush());
                }
                last_update = std::time::Instant::now();
                needs_update = false;
            }
        }
    }
}

fn render_progress_line(start: std::time::Instant, line: &str) -> String {
    let elapsed = start.elapsed().as_secs();
    let secs = elapsed % 60;
    let mins = (elapsed / 60) % 60;
    let hours = elapsed / 3600;

    format!("[{hours:02}:{mins:02}:{secs:02}] {line}")
}

fn handle_err<T>(r: std::io::Result<T>) -> T {
    r.expect("stderr write failed")
}

fn duration_to_next_second(duration: Duration) -> Duration {
    let subsec_nanos = duration.subsec_nanos();
    if subsec_nanos == 0 {
        Duration::ZERO
    } else {
        Duration::from_nanos((1_000_000_000 - subsec_nanos).into())
    }
}

enum Command {
    Finish,
    PrintRawLine(Vec<u8>),
    SetProgress(String),
    FreezeProgress,
}

pub(crate) struct Handle {
    join_handle: std::thread::JoinHandle<()>,
    sender: mpsc::Sender<Command>,
}

impl Handle {
    pub(crate) fn finish(self) {
        self.sender
            .send(Command::Finish)
            .expect("term out endpoint closed");
        self.join_handle.join().expect("term out thread panicked");
    }

    pub(crate) fn get_progress_print(&self) -> ProgressPrint {
        ProgressPrint {
            sender: self.sender.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ProgressPrint {
    sender: mpsc::Sender<Command>,
}

impl ProgressPrint {
    pub(crate) fn set_progress(&self, progress: String) {
        self.sender
            .send(Command::SetProgress(progress))
            .expect("term out endpoint closed");
    }

    pub(crate) fn freeze_progress(&self) {
        self.sender
            .send(Command::FreezeProgress)
            .expect("term out endpoint closed");
    }

    pub(crate) fn print_raw_line(&self, line: Vec<u8>) {
        self.sender
            .send(Command::PrintRawLine(line))
            .expect("term out endpoint closed");
    }
}
