use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to keep reading output after the process exits. A child it spawned
/// (mage's queue worker) can inherit the pipes and hold them open long after.
const PIPE_GRACE: Duration = Duration::from_secs(2);

pub struct Output {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn combined(&self) -> String {
        match (self.stdout.trim().is_empty(), self.stderr.trim().is_empty()) {
            (_, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout.trim_end(), self.stderr),
        }
    }

    pub fn failure_text(&self) -> String {
        let text = self.combined();
        let text = text.trim();
        if text.is_empty() {
            format!("exit code {:?}", self.code)
        } else {
            text.to_string()
        }
    }
}

pub fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    hide_window(&mut cmd);
    cmd
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

pub fn run(mut cmd: Command, stdin: Option<&str>, timeout: Duration) -> std::io::Result<Output> {
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break (child.wait().ok(), true);
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let pipes_close_by = Instant::now() + PIPE_GRACE;
    Ok(Output {
        success: !timed_out && status.is_some_and(|s| s.success()),
        code: status.and_then(|s| s.code()),
        stdout: stdout.finish(pipes_close_by),
        stderr: stderr.finish(pipes_close_by),
        timed_out,
    })
}

struct Drain {
    bytes: Arc<Mutex<Vec<u8>>>,
    eof: mpsc::Receiver<()>,
}

impl Drain {
    fn finish(self, deadline: Instant) -> String {
        let _ = self
            .eof
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let bytes = self.bytes.lock().unwrap_or_else(|p| p.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Drain {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (tx, eof) = mpsc::channel();
    let sink = bytes.clone();
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 8192];
            while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                sink.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend_from_slice(&chunk[..n]);
            }
        }
        let _ = tx.send(());
    });
    Drain { bytes, eof }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn returns_once_the_process_exits_even_if_a_grandchild_holds_the_pipe() {
        let mut cmd = command(Path::new("cmd"));
        cmd.args(["/C", "echo done& start /b ping -n 15 127.0.0.1 >nul"]);
        let started = Instant::now();
        let output = run(cmd, None, Duration::from_secs(30)).unwrap();
        assert!(output.success);
        assert_eq!(output.stdout.trim(), "done");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= PIPE_GRACE,
            "the grandchild should hold the pipe: {elapsed:?}"
        );
        assert!(elapsed < PIPE_GRACE * 2, "{elapsed:?}");
    }
}
