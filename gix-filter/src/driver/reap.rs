//! Terminating filter child processes and waiting for them.
//!
//! On Unix a child that exited but was never waited for keeps occupying a slot in the process table
//! until its parent exits. A long-running application that filters therefore has to reap every child
//! it spawns, or it eventually exhausts the per-user process limit, at which point nothing that user
//! runs can `fork()` anymore.

use std::time::{Duration, Instant};

/// How long a filter process may take to exit on its own after its input and output were closed, before
/// it is killed. Only a process that ignores the closure of its input can reach this.
const TERMINATION_GRACE: Duration = Duration::from_secs(1);

/// The longest a single poll of a terminating process waits before asking again.
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Wait for `child` to exit, and kill it if it doesn't do so within [`TERMINATION_GRACE`], so that
/// dropping its owner can neither leak a process nor block on one that refuses to exit.
///
/// The caller is expected to have closed the child's input and output already, which is what tells it
/// to shut down, so it is typically gone after the first poll.
pub(crate) fn terminate_and_reap(mut child: std::process::Child) {
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
    kill_and_reap(child);
}

/// Kill `child` and wait for it, for when there is nothing left to wait for it to do.
///
/// Killing on its own is not enough: a killed child still holds its slot in the process table until
/// somebody waits for it.
pub(crate) fn kill_and_reap(mut child: std::process::Child) {
    child.kill().ok();
    child.wait().ok();
}

/// A child process that is [terminated and waited for][terminate_and_reap()] when dropped, unless it
/// was [given up][OnDrop::take()] before that.
pub(crate) struct OnDrop(Option<std::process::Child>);

impl OnDrop {
    /// Take ownership of `child` so it is reaped once this instance goes out of scope.
    pub(crate) fn new(child: std::process::Child) -> Self {
        OnDrop(Some(child))
    }

    /// Return the process id of the child.
    pub(crate) fn id(&self) -> u32 {
        self.0.as_ref().expect("owned until given up").id()
    }

    /// Hand the child back out, leaving its fate to the caller instead of reaping it on drop.
    pub(crate) fn take(&mut self) -> std::process::Child {
        self.0.take().expect("a child is given up at most once")
    }
}

impl Drop for OnDrop {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            terminate_and_reap(child);
        }
    }
}
