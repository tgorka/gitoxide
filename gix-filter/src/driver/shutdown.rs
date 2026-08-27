use std::time::{Duration, Instant};

use bstr::BString;

use crate::driver::{Running, State};

///
#[derive(Debug, Copy, Clone)]
pub enum Mode {
    /// Wait for long-running processes after signaling them to shut down by closing their input and output.
    ///
    /// Note that this waits without a time limit, unlike the automatic cleanup performed when a
    /// [`State`] is dropped, so a process that ignores the closure of its input can block indefinitely.
    WaitForProcesses,
    /// Do not do anything with long-running processes, which typically allows them to keep running or shut down on their own time.
    /// This is the fastest mode as no synchronization happens at all.
    ///
    /// Note that on Unix this leaves each child in the process table until this process exits, as nobody
    /// waits for it. Prefer [`WaitForProcesses`][Mode::WaitForProcesses] in long-running applications.
    Ignore,
}

/// Lifecycle
impl State {
    /// Handle long-running processes according to `mode`.
    /// Return a list of `(process, Option<status>)`
    ///
    /// This is the owned form of [`shutdown_mut()`][State::shutdown_mut()]. Note that dropping a
    /// [`State`] also reaps every process it still owns, so calling this is only needed to learn how
    /// they exited or to choose a different [`Mode`].
    pub fn shutdown(mut self, mode: Mode) -> Result<Vec<(BString, Option<std::process::ExitStatus>)>, std::io::Error> {
        self.shutdown_mut(mode)
    }

    /// Handle long-running processes according to `mode`, leaving `self` without any of them.
    /// Return a list of `(process, Option<status>)`
    ///
    /// Under [`Mode::WaitForProcesses`], every process is waited for even if waiting for one of them
    /// fails, so a single failure can't leave the remaining ones unreaped. The first error is returned
    /// once all of them were dealt with.
    pub fn shutdown_mut(
        &mut self,
        mode: Mode,
    ) -> Result<Vec<(BString, Option<std::process::ExitStatus>)>, std::io::Error> {
        let running = std::mem::take(&mut *self.running);
        let mut out = Vec::with_capacity(running.len());
        let mut first_err = None;
        for (cmd, client) in running {
            match mode {
                Mode::WaitForProcesses => {
                    let mut child = client.into_child();
                    match child.wait() {
                        Ok(status) => out.push((cmd, Some(status))),
                        Err(err) => {
                            out.push((cmd, None));
                            first_err = first_err.or(Some(err));
                        }
                    }
                }
                Mode::Ignore => {
                    out.push((cmd, None));
                }
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(out),
        }
    }
}

/// How long a filter process may take to exit on its own after its input and output were closed, before
/// it is killed. Only a process that ignores the closure of its input can reach this.
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

/// The longest a single poll of a terminating process waits before asking again.
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(50);

impl Drop for Running {
    fn drop(&mut self) {
        for (_cmd, client) in std::mem::take(&mut self.0) {
            terminate_and_reap(client.into_child());
        }
    }
}

/// Wait for `child` to exit, and kill it if it doesn't do so within [`TERMINATION_GRACE`], so that
/// dropping a [`State`] can neither leak a process nor block on one that refuses to exit.
///
/// The caller is expected to have closed the child's input and output already, which is what tells it
/// to shut down, so it is typically gone after the first poll.
fn terminate_and_reap(mut child: std::process::Child) {
    let start = Instant::now();
    let mut poll_interval = Duration::from_millis(1);
    loop {
        match child.try_wait() {
            // It was reaped, or it can't be and retrying won't change that.
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if start.elapsed() >= TERMINATION_GRACE {
            break;
        }
        std::thread::sleep(poll_interval);
        poll_interval = (poll_interval * 2).min(MAX_POLL_INTERVAL);
    }
    child.kill().ok();
    child.wait().ok();
}
