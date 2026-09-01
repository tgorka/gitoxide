//! Observe whether a child process was reaped, to pin the lifecycle of long-running `process` filters.
//!
//! On Unix a child that exits but is never `wait()`ed for stays in the process table as a zombie,
//! owned by its parent until the parent itself exits. Nothing in `std` can observe that: `try_wait()`
//! needs the `Child` handle, and holding one means the process wasn't abandoned in the first place.
//! Hence these helpers, which ask the kernel by process id instead.

use std::time::{Duration, Instant};

/// What the operating system knows about a process that was launched by this test binary.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Child {
    /// It is still running as it didn't exit yet.
    Running,
    /// It exited, but nobody waited for it, so it still occupies a slot in the process table.
    ///
    /// Note that observing this state is also what collects the process, as `waitpid()` has no way
    /// to ask without reaping.
    Unreaped,
    /// It isn't a child of this process anymore because it was waited for.
    Reaped,
}

/// How long to give a child to notice that its input and output were closed.
///
/// Only a leaked child is ever waited for here, so this is a failure timeout, not a delay we pay
/// on the happy path.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Ask the kernel about `pid` without blocking.
pub(crate) fn observe(pid: u32) -> Child {
    let mut status = 0;
    // SAFETY: `waitpid()` writes to `status` only, and it is valid for the duration of the call.
    let res = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    match res {
        0 => Child::Running,
        res if res > 0 => Child::Unreaped,
        _ => {
            let err = std::io::Error::last_os_error();
            assert_eq!(
                err.raw_os_error(),
                Some(libc::ECHILD),
                "`waitpid({pid})` may only fail because the process isn't ours to wait for, but failed with {err}"
            );
            Child::Reaped
        }
    }
}

/// Ask the kernel about `pid` until it stops [running][Child::Running], and return what it settled on.
///
/// A child that the library waited for is observable right away, as reaping implies it had already
/// exited. A leaked one first needs a moment to notice that its input was closed, hence the polling.
pub(crate) fn settle(pid: u32) -> Child {
    let start = Instant::now();
    loop {
        match observe(pid) {
            Child::Running if start.elapsed() < EXIT_TIMEOUT => std::thread::sleep(Duration::from_millis(5)),
            state => return state,
        }
    }
}

/// Return the process id of the one direct child of this process whose command line contains `marker`.
///
/// [`observe()`] needs a process id, and the single-file filter path never hands one out: its child is
/// owned by a reader deep inside the library. So it is looked up in the process table instead,
/// discriminated by a marker the caller planted on the driver's command line - unlike a plain
/// "our newest child" heuristic, that stays correct while other tests spawn filters of their own.
pub(crate) fn child_with_marker(marker: &str) -> u32 {
    let ours = std::process::id().to_string();
    let ps = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=,ppid=,args="])
        .output()
        .expect("`ps` is available on every unix");
    assert!(ps.status.success(), "`ps` failed: {:?}", ps.status);
    let listing = String::from_utf8_lossy(&ps.stdout);
    let mut found: Vec<u32> = listing
        .lines()
        .filter(|line| line.contains(marker))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?;
            (fields.next()? == ours).then(|| pid.parse().expect("`ps` prints numeric process ids"))
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one child of ours should run the driver marked with {marker:?}, but found {found:?}"
    );
    found.pop().expect("just asserted")
}

/// Each of these pins one way for a filter child to end up unreaped, all of which were leaks once. See
/// [`crate::driver::shutdown`] for the lifecycle of a [`gix_filter::driver::State`] as a whole.
mod tests {
    use std::{
        io::Read,
        time::{Duration, Instant},
    };

    use bstr::ByteVec;
    use gix_filter::driver::{Operation, apply::Delay};

    use crate::{
        driver::{
            apply::{context_from_path, driver_no_process, driver_with_process, extract_delayed_key},
            driver_path,
            shutdown::extract_client,
        },
        reap::{Child, child_with_marker, observe},
    };

    /// A filter that dies mid-invocation makes the next read or write fail with a broken pipe, upon
    /// which its client is dropped from the set of running processes. Dropping it is what has to reap
    /// the child, which is the leak that fires most often in the field: it needs nothing but a filter
    /// that crashes.
    #[test]
    fn a_client_removed_after_an_io_error_is_reaped() -> crate::Result {
        let mut state = gix_filter::driver::State::default();
        let driver = driver_with_process();
        let pid = extract_client(state.maybe_launch_process(&driver, Operation::Clean, "does not matter".into())?).id();
        assert_eq!(observe(pid), Child::Running, "the filter is up and is a child of ours");

        // The arrow filter panics when asked to filter a path ending in "fail", taking the process with it.
        match state.apply(
            &driver,
            &mut std::io::empty(),
            Operation::Smudge,
            context_from_path("fail"),
        ) {
            Ok(_) => unreachable!("the filter dies rather than answering"),
            Err(err) => assert!(
                matches!(err, gix_filter::driver::apply::Error::ProcessInvoke { .. }),
                "{err:?}: the invocation is what notices that the process is gone"
            ),
        }

        assert_eq!(
            observe(pid),
            Child::Reaped,
            "the client that was removed in response was waited for, not merely forgotten"
        );
        Ok(())
    }

    /// An unknown status is answered by killing the filter, and a killed child holds its slot in the
    /// process table just like one that exited on its own until somebody waits for it.
    #[test]
    fn a_client_killed_over_a_strange_status_is_reaped() -> crate::Result {
        let mut state = gix_filter::driver::State::default();
        let driver = driver_with_process();
        let client = extract_client(state.maybe_launch_process(&driver, Operation::Clean, "does not matter".into())?);
        let pid = client.id();
        assert!(
            client
                .invoke(
                    "next-invocation-returns-strange-status-and-smudge-fails-permanently",
                    &mut None.into_iter(),
                    &mut &b""[..]
                )?
                .is_success()
        );

        match state.apply(
            &driver,
            &mut std::io::empty(),
            Operation::Smudge,
            context_from_path("any"),
        ) {
            Ok(_) => unreachable!("the strange status is an error"),
            Err(err) => assert!(
                matches!(err, gix_filter::driver::apply::Error::ProcessStatus { status, .. } if status.message() == Some("send-term-signal")),
                "the filter asked to be terminated"
            ),
        }

        assert_eq!(
            observe(pid),
            Child::Reaped,
            "killing the filter was followed by waiting for it"
        );
        Ok(())
    }

    /// The same kill-on-strange-status exists on the delayed path, once for fetching a delayed result…
    #[test]
    fn a_client_killed_while_fetching_a_delayed_path_is_reaped() -> crate::Result {
        let mut state = gix_filter::driver::State::default();
        let driver = driver_with_process();
        let pid =
            extract_client(state.maybe_launch_process(&driver, Operation::Smudge, "does not matter".into())?).id();

        let key = extract_delayed_key(state.apply_delayed(
            &driver,
            &mut &b"hello\n"[..],
            Operation::Smudge,
            Delay::Allow,
            context_from_path("sub/a.txt"),
        )?);
        assert!(
            extract_client(state.maybe_launch_process(&driver, Operation::Smudge, "does not matter".into())?)
                .invoke(
                    "next-invocation-returns-strange-status-and-smudge-fails-permanently",
                    &mut None.into_iter(),
                    &mut &b""[..]
                )?
                .is_success()
        );

        match state.fetch_delayed(&key, "sub/a.txt".into(), Operation::Smudge) {
            Ok(_) => unreachable!("the filter answers the fetch with a status that asks to be terminated"),
            Err(err) => assert!(
                matches!(err, gix_filter::driver::delayed::fetch::Error::ProcessStatus { status, .. } if status.message() == Some("send-term-signal")),
            ),
        }

        assert_eq!(
            observe(pid),
            Child::Reaped,
            "killing the filter was followed by waiting for it"
        );
        Ok(())
    }

    /// …and once for listing them.
    #[test]
    fn a_client_killed_while_listing_delayed_paths_is_reaped() -> crate::Result {
        let mut state = gix_filter::driver::State::default();
        let driver = driver_with_process();
        let pid =
            extract_client(state.maybe_launch_process(&driver, Operation::Smudge, "does not matter".into())?).id();

        let key = extract_delayed_key(state.apply_delayed(
            &driver,
            &mut &b"hello\n"[..],
            Operation::Smudge,
            Delay::Allow,
            context_from_path("sub/a.txt"),
        )?);
        assert!(
            extract_client(state.maybe_launch_process(&driver, Operation::Smudge, "does not matter".into())?)
                .invoke("next-list-returns-strange-status", &mut None.into_iter(), &mut &b""[..])?
                .is_success()
        );

        let err = state
            .list_delayed_paths(&key)
            .expect_err("the filter answers the listing with a status that asks to be terminated");
        assert!(
            matches!(&err, gix_filter::driver::delayed::list::Error::ProcessStatus { status } if status.message() == Some("send-term-signal")),
            "{err:?}"
        );

        assert_eq!(
            observe(pid),
            Child::Reaped,
            "killing the filter was followed by waiting for it"
        );
        Ok(())
    }

    /// A single-file filter is waited for by the reader that streams its output, but only once that
    /// reader reaches the end. A caller that stops early - or an unwind that drops the reader on the way
    /// out, as an interrupted status walk does - has to reap it too.
    #[test]
    fn an_abandoned_single_file_filter_output_reaps_its_child() -> crate::Result {
        /// Unique to this test, so the child can be told apart from filters other tests are running.
        const MARKER: &str = "reap-abandoned-single-file.txt";

        let mut state = gix_filter::driver::State::default();
        let driver = driver_no_process();
        assert!(driver.required, "a required driver streams instead of buffering");

        let mut filtered = state
            .apply(
                &driver,
                &mut &vec![b'x'; 256 * 1024][..],
                Operation::Clean,
                context_from_path(MARKER),
            )?
            .expect("filter present");

        let mut head = [0u8; 100];
        filtered.read_exact(&mut head)?;
        let pid = child_with_marker(MARKER);

        drop(filtered);

        assert_eq!(
            observe(pid),
            Child::Reaped,
            "abandoning the reader terminated the filter and waited for it, rather than leaving it behind"
        );
        Ok(())
    }

    /// Reaping means waiting, and waiting for a filter that doesn't notice its closed input would turn
    /// the leak into a hang - no improvement, and not something a caller of `Repository::status()` or of
    /// checkout could opt out of, as they drop their pipeline internally. So the wait is bounded and the
    /// process is killed once its grace period is up.
    ///
    /// Note that the driver command is a quoted path, so it needs a shell and the child that is owned
    /// here is that shell — which is what these assertions are about.
    #[test]
    fn a_filter_that_ignores_its_closed_input_is_gone_within_the_grace_period() -> crate::Result {
        let mut driver = driver_with_process();
        driver.process = Some({
            let mut command = driver_path();
            command.push_str(" process-ignores-eof");
            command
        });

        let mut state = gix_filter::driver::State::default();
        let pid = extract_client(state.maybe_launch_process(&driver, Operation::Clean, "does not matter".into())?).id();
        assert_eq!(
            observe(pid),
            Child::Running,
            "the filter completed its handshake and is now waiting for work it will never get"
        );

        let start = Instant::now();
        drop(state);
        let elapsed = start.elapsed();

        assert_eq!(
            observe(pid),
            Child::Reaped,
            "the spawned process was killed and waited for, rather than left behind"
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "the drop took {elapsed:?}, so it waited for the filter instead of terminating it \
             (the grace period is a second, and this filter stays alive for ten)"
        );
        Ok(())
    }
}
