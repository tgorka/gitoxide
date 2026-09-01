use bstr::BString;

use crate::driver::State;

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
        let running = std::mem::take(&mut self.running);
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
                    // Hand the child out of the client so dropping it can't wait for it after all,
                    // which is precisely what this mode is for.
                    drop(client.into_child());
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
