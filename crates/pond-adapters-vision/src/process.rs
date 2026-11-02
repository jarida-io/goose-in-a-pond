//! A capture child process (ffmpeg or gst-launch): stdout carries frames, stderr explains failures.

use std::ffi::OsStr;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::unix::pipe::Receiver;
use tokio::process::{Child, ChildStdout, Command};

/// How long a killed capture may take to release its camera before a reopen goes ahead anyway.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Enough of stderr for the last error line or two.
const STDERR_TAIL_BYTES: usize = 2048;

/// stdout can close before the last stderr line has been read; this bounds the wait for it.
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

pub struct CaptureProcess {
    child: Child,
    pub stdout: ChildStdout,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    program: &'static str,
    secrets: Vec<String>,
}

impl CaptureProcess {
    /// `input` is the camera address, whose credentials never reach an error message.
    pub fn spawn(bin: &OsStr, args: &[String], program: &'static str, input: &str) -> Result<Self> {
        Self::start(bin, args, program, input, Stdio::null())
    }

    /// [`CaptureProcess::spawn`] with a second stream out of the child, which writes it to its
    /// fd 0: a pipe in place of stdin.
    pub fn spawn_with_second_stream(
        bin: &OsStr,
        args: &[String],
        program: &'static str,
        input: &str,
    ) -> Result<(Self, Receiver)> {
        let (reader, writer) =
            std::io::pipe().context("no pipe for the capture's second stream")?;
        let process = Self::start(bin, args, program, input, Stdio::from(writer))?;
        let second = Receiver::from_owned_fd(OwnedFd::from(reader))
            .context("the capture's second stream cannot be read")?;
        Ok((process, second))
    }

    fn start(
        bin: &OsStr,
        args: &[String],
        program: &'static str,
        input: &str,
        stdin: Stdio,
    ) -> Result<Self> {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to start {program} ({bin:?}); is it installed?"))?;
        let stdout = child
            .stdout
            .take()
            .with_context(|| format!("{program} stdout was not captured"))?;
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(keep_tail(stderr, stderr_tail.clone())));
        Ok(Self {
            child,
            stdout,
            stderr_tail,
            stderr_task,
            program,
            secrets: credentials_in(input),
        })
    }

    /// Why the process stopped, from its last stderr line, with credentials removed.
    pub async fn exit_reason(&mut self) -> String {
        if let Some(task) = self.stderr_task.take() {
            let _ = tokio::time::timeout(STDERR_DRAIN_TIMEOUT, task).await;
        }
        let tail = self
            .stderr_tail
            .lock()
            .map(|t| String::from_utf8_lossy(&t).into_owned())
            .unwrap_or_default();
        let last = tail
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("");
        if last.is_empty() {
            format!("{} stopped", self.program)
        } else {
            format!("{}: {}", self.program, redact(last, &self.secrets))
        }
    }

    /// Kill and reap, so the camera is free when this returns (or after [`CLOSE_TIMEOUT`]).
    pub async fn close(&mut self) {
        let _ = self.child.start_kill();
        if tokio::time::timeout(CLOSE_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                program = self.program,
                "capture process did not exit in time"
            );
        }
    }
}

async fn keep_tail(mut stderr: impl AsyncRead + Unpin, tail: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 1024];
    while let Ok(n) = stderr.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        if let Ok(mut t) = tail.lock() {
            t.extend_from_slice(&chunk[..n]);
            let excess = t.len().saturating_sub(STDERR_TAIL_BYTES);
            t.drain(..excess);
        }
    }
}

/// The userinfo and password of a URL, which a tool may echo in its own words.
fn credentials_in(input: &str) -> Vec<String> {
    let Some((_, rest)) = input.split_once("://") else {
        return Vec::new();
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let Some(at) = authority.rfind('@') else {
        return Vec::new();
    };
    let userinfo = &authority[..at];
    let mut secrets = vec![userinfo.to_string()];
    // A shorter password would blank out ordinary words in the message.
    if let Some((_, password)) = userinfo.split_once(':').filter(|(_, p)| p.len() >= 3) {
        secrets.push(password.to_string());
    }
    secrets.retain(|s| !s.is_empty());
    secrets
}

/// Replaces every URL's userinfo, and each known secret, with `***`.
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"' || c == '\''
            })
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(at) => {
                out.push_str("***@");
                out.push_str(&authority[at + 1..]);
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    for secret in secrets {
        out = out.replace(secret.as_str(), "***");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_survive_redaction() {
        let secrets = credentials_in("rtsp://admin:hunter2@cam.local:554/live");
        assert_eq!(secrets, vec!["admin:hunter2", "hunter2"]);
        let line =
            "[rtsp @ 0x1] rtsp://admin:hunter2@cam.local:554/live: Connection refused (hunter2)";
        let clean = redact(line, &secrets);
        assert!(!clean.contains("hunter2"), "{clean}");
        assert!(clean.contains("rtsp://***@cam.local:554/live: Connection refused"));
        assert_eq!(
            redact("tcp://127.0.0.1:9 failed", &[]),
            "tcp://127.0.0.1:9 failed"
        );
        assert!(credentials_in("/dev/video0").is_empty());
    }

    #[tokio::test]
    async fn a_failed_process_explains_itself_and_close_reaps_it() {
        let mut p = CaptureProcess::spawn(
            OsStr::new("sh"),
            &[
                "-c".into(),
                "echo 'first line' >&2; echo 'rtsp://u:pw@h/x: 404 Not Found' >&2; exit 1".into(),
            ],
            "ffmpeg",
            "rtsp://u:pw@h/x",
        )
        .unwrap();
        let mut out = Vec::new();
        p.stdout.read_to_end(&mut out).await.unwrap();
        assert_eq!(
            p.exit_reason().await,
            "ffmpeg: rtsp://***@h/x: 404 Not Found"
        );

        let mut running = CaptureProcess::spawn(
            OsStr::new("sh"),
            &["-c".into(), "exec sleep 30".into()],
            "gst-launch-1.0",
            "/dev/video0",
        )
        .unwrap();
        let started = std::time::Instant::now();
        running.close().await;
        assert!(started.elapsed() < CLOSE_TIMEOUT, "killed, not timed out");
        assert!(running.child.try_wait().unwrap().is_some(), "reaped");
        assert_eq!(running.exit_reason().await, "gst-launch-1.0 stopped");
    }
}
