use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::io::{BufReader, BufWriter};
use futures::{
    AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, Stream, StreamExt as _,
};
use gpui::{AsyncApp, Task};

use util::TryFutureExt as _;
use util::process::Child;
use util::shell::Shell;
use util::shell_builder::ShellBuilder;

use crate::client::ModelContextServerBinary;
use crate::transport::Transport;

/// How often to re-check for the exit status of a server that has closed its
/// stdout, and how many times before giving up and leaving it to the
/// background reaper. 100 x 100ms is ten seconds.
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(100);
const REAP_ATTEMPTS: usize = 100;

pub struct StdioTransport {
    stdout_sender: async_channel::Sender<String>,
    stdin_receiver: async_channel::Receiver<String>,
    stderr_receiver: async_channel::Receiver<String>,
    /// Shared so the exit watcher can collect the process without taking
    /// ownership away from `Drop`, which still has to be able to kill it.
    /// The lock is never held across an await.
    server: Arc<Mutex<Child>>,
    /// Dropped with the transport, so a watcher never outlives what it watches.
    _exit_watch: Task<()>,
}

impl StdioTransport {
    pub fn new(
        binary: ModelContextServerBinary,
        working_directory: &Option<PathBuf>,
        cx: &AsyncApp,
    ) -> Result<Self> {
        let builder = ShellBuilder::new(&Shell::System, cfg!(windows)).non_interactive();
        let mut command =
            builder.build_std_command(Some(binary.executable.display().to_string()), &binary.args);

        command.envs(binary.env.unwrap_or_default());

        if let Some(working_directory) = working_directory {
            command.current_dir(working_directory);
        }

        let mut server = Child::spawn(
            command,
            std::process::Stdio::piped(),
            std::process::Stdio::piped(),
            std::process::Stdio::piped(),
        )?;

        let stdin = server.stdin.take().unwrap();
        let stdout = server.stdout.take().unwrap();
        let stderr = server.stderr.take().unwrap();

        let server = Arc::new(Mutex::new(server));

        let (stdin_sender, stdin_receiver) = async_channel::unbounded::<String>();
        let (stdout_sender, stdout_receiver) = async_channel::unbounded::<String>();
        let (stderr_sender, stderr_receiver) = async_channel::unbounded::<String>();
        let (stdout_closed_tx, stdout_closed_rx) = futures::channel::oneshot::channel();

        cx.spawn(async move |_| Self::handle_output(stdin, stdout_receiver).log_err().await)
            .detach();

        cx.spawn(async move |_| Self::handle_input(stdout, stdin_sender, stdout_closed_tx).await)
            .detach();

        cx.spawn(async move |_| Self::handle_err(stderr, stderr_sender).await)
            .detach();

        let exit_watch = cx.spawn({
            let server = server.clone();
            async move |cx| Self::reap_after_exit(server, stdout_closed_rx, cx).await
        });

        Ok(Self {
            stdout_sender,
            stdin_receiver,
            stderr_receiver,
            server,
            _exit_watch: exit_watch,
        })
    }

    /// Collect a server that exited on its own while the transport was still
    /// alive. Nothing else waits on the child, and `async-process` only reaps one
    /// once its `Child` handle is dropped, so without this an exited server stays
    /// a zombie for as long as the window lives. Servers that shut themselves
    /// down when idle make this routine rather than exceptional.
    ///
    /// Polls instead of awaiting `status()` because `Drop` needs the same lock to
    /// kill the server, and `status()` would hold it for the entire wait.
    async fn reap_after_exit(
        server: Arc<Mutex<Child>>,
        stdout_closed: futures::channel::oneshot::Receiver<()>,
        cx: &mut AsyncApp,
    ) {
        if stdout_closed.await.is_err() {
            // Sender dropped without signalling: the transport is going away and
            // `Drop` owns the shutdown.
            return;
        }

        for _ in 0..REAP_ATTEMPTS {
            let status = server
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .try_status();
            match status {
                Ok(Some(status)) => {
                    log::warn!("MCP server exited on its own with status {status}");
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    log::warn!("failed to collect exit status of MCP server: {error}");
                    return;
                }
            }
            cx.background_executor().timer(REAP_POLL_INTERVAL).await;
        }

        log::warn!(
            "MCP server closed stdout but has not exited; leaving it to the background reaper"
        );
    }

    async fn handle_input<Stdout>(
        stdin: Stdout,
        inbound_rx: async_channel::Sender<String>,
        stdout_closed: futures::channel::oneshot::Sender<()>,
    ) where
        Stdout: AsyncRead + Unpin + Send + 'static,
    {
        let mut stdin = BufReader::new(stdin);
        let mut line = String::new();
        while let Ok(n) = stdin.read_line(&mut line).await {
            if n == 0 {
                break;
            }
            if inbound_rx.send(line.clone()).await.is_err() {
                break;
            }
            line.clear();
        }
        // Stdout is closed, so the server is on its way out (or already gone).
        let _ = stdout_closed.send(());
    }

    async fn handle_output<Stdin>(
        stdin: Stdin,
        outbound_rx: async_channel::Receiver<String>,
    ) -> Result<()>
    where
        Stdin: AsyncWrite + Unpin + Send + 'static,
    {
        let mut stdin = BufWriter::new(stdin);
        let mut pinned_rx = Box::pin(outbound_rx);
        while let Some(message) = pinned_rx.next().await {
            log::trace!("outgoing message: {}", message);

            stdin.write_all(message.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }
        Ok(())
    }

    async fn handle_err<Stderr>(stderr: Stderr, stderr_tx: async_channel::Sender<String>)
    where
        Stderr: AsyncRead + Unpin + Send + 'static,
    {
        let mut stderr = BufReader::new(stderr);
        let mut line = String::new();
        while let Ok(n) = stderr.read_line(&mut line).await {
            if n == 0 {
                break;
            }
            if stderr_tx.send(line.clone()).await.is_err() {
                break;
            }
            line.clear();
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, message: String) -> Result<()> {
        Ok(self.stdout_sender.send(message).await?)
    }

    fn receive(&self) -> Pin<Box<dyn Stream<Item = String> + Send>> {
        Box::pin(self.stdin_receiver.clone())
    }

    fn receive_err(&self) -> Pin<Box<dyn Stream<Item = String> + Send>> {
        Box::pin(self.stderr_receiver.clone())
    }
}

#[cfg(test)]
impl StdioTransport {
    fn server_pid(&self) -> u32 {
        self.server
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .id()
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Killing synchronously here is the guarantee that a server never
        // outlives its transport; the eventual reap is handled by
        // `async-process` when the `Child` is dropped alongside us.
        let _ = self
            .server
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    /// A collected process has no procfs entry at all. Deliberately *not*
    /// "is not in state Z": a still-running process is also not a zombie, so
    /// that predicate would be satisfied before the server had even exited and
    /// the test would pass without proving anything.
    fn is_collected(pid: u32) -> bool {
        !std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    #[gpui::test]
    async fn server_that_exits_on_its_own_is_reaped(cx: &mut TestAppContext) {
        // Servers with an idle-shutdown watchdog exit while the transport is
        // still alive. `async-process` only reaps a child once its `Child` handle
        // is dropped, so before the exit watcher existed such a server sat in `Z`
        // state for the lifetime of the window.
        cx.executor().allow_parking();

        let binary = ModelContextServerBinary {
            executable: "sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            env: None,
            timeout: None,
        };
        let transport = cx
            .update(|cx| StdioTransport::new(binary, &None, &cx.to_async()))
            .expect("spawning `sh -c 'exit 0'` should succeed");
        let pid = transport.server_pid();

        // This test straddles two clocks: the child is a real OS process that
        // needs wall time to exit and close its pipes, while gpui's test executor
        // runs on a virtual clock. So each turn of the loop advances both — real
        // time for the process, `run_until_parked` plus `advance_clock` for the
        // watcher task and its poll timer.
        //
        // Without the watcher the exited process keeps its procfs entry in state Z
        // indefinitely, so this loop times out and the assertion fires.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut collected = false;
        while std::time::Instant::now() < deadline {
            cx.executor().run_until_parked();
            cx.executor().advance_clock(REAP_POLL_INTERVAL);
            if is_collected(pid) {
                collected = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        assert!(
            collected,
            "server pid {pid} was left unreaped instead of being collected"
        );
    }
}
