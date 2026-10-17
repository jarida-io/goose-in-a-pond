//! Starting the helper process, behind a seam so the supervisor is tested over in-memory pipes.

use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};

/// The subcommand `pond-server` runs the helper under.
pub const HELPER_SUBCOMMAND: &str = "ble-helper";

/// How long exit waits for the helper's stderr to close once the process is gone.
const STDERR_DRAIN: Duration = Duration::from_secs(2);

/// One running helper. Dropping `exited` before it resolves kills the process.
pub struct HelperProcess {
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    /// Resolves once the helper is gone and its stderr has closed.
    pub exited: Pin<Box<dyn Future<Output = HelperExit> + Send>>,
}

/// How the helper ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// Its last stderr line, which is usually the fatal one.
    pub last_words: Option<String>,
}

pub trait HelperLauncher: Send + Sync {
    fn launch(&self) -> io::Result<HelperProcess>;
}

/// The real helper: a child process whose stdin and stdout carry the protocol.
pub struct ProcessLauncher {
    program: PathBuf,
    args: Vec<OsString>,
}

impl ProcessLauncher {
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// This very binary's `ble-helper`, found the way the server finds its other children.
    pub fn current_exe() -> io::Result<Self> {
        Ok(Self::new(std::env::current_exe()?, [HELPER_SUBCOMMAND]))
    }
}

impl HelperLauncher for ProcessLauncher {
    fn launch(&self) -> io::Result<HelperProcess> {
        let mut child = tokio::process::Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let missing = |name: &str| io::Error::other(format!("the helper's {name} was not piped"));
        let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| missing("stderr"))?;

        let last_words = Arc::new(Mutex::new(None::<String>));
        let drain = {
            let last_words = last_words.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "giap::ble_helper", "{line}");
                    *last_words.lock().unwrap_or_else(|e| e.into_inner()) = Some(line);
                }
            })
        };
        let exited = Box::pin(async move {
            let status = child.wait().await;
            // Close, not exit: the last stderr line may still be in flight when the process ends.
            let _ = tokio::time::timeout(STDERR_DRAIN, drain).await;
            let last_words = last_words.lock().unwrap_or_else(|e| e.into_inner()).take();
            match status {
                Ok(status) => HelperExit {
                    code: status.code(),
                    signal: signal_of(&status),
                    last_words,
                },
                Err(error) => HelperExit {
                    last_words: Some(format!("could not wait for the helper: {error}")),
                    ..HelperExit::default()
                },
            }
        });
        Ok(HelperProcess {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            exited,
        })
    }
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::*;

    #[tokio::test]
    async fn a_process_reports_its_exit_and_its_last_stderr_line() {
        let launcher = ProcessLauncher::new(
            "/bin/sh",
            [
                "-c",
                r#"echo '{"event":"ready","protocol":1}'; echo starting >&2; echo 'no radio' >&2; exit 3"#,
            ],
        );
        let mut process = launcher.launch().unwrap();
        let mut stdout = String::new();
        process.stdout.read_to_string(&mut stdout).await.unwrap();
        assert_eq!(stdout, "{\"event\":\"ready\",\"protocol\":1}\n");
        assert_eq!(
            process.exited.await,
            HelperExit {
                code: Some(3),
                signal: None,
                last_words: Some("no radio".into()),
            }
        );
    }

    #[tokio::test]
    async fn a_killed_process_reports_the_signal() {
        let launcher = ProcessLauncher::new("/bin/sh", ["-c", "kill -9 $$"]);
        let exit = launcher.launch().unwrap().exited.await;
        assert_eq!((exit.code, exit.signal), (None, Some(9)));
    }
}
