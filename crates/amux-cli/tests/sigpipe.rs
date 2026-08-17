//! AMUX-2653: `amux-rs <verb> | head` must not panic.
//!
//! Rust sets SIGPIPE to SIG_IGN before `main`, so writes to a closed pipe return
//! EPIPE and every bare `println!` unwraps it into a panic. `amux-rs board list |
//! head -2` exited 101 and printed 254 bytes of panic noise to stderr.
//!
//! `main` now restores SIG_DFL, so the process dies quietly on EPIPE the way
//! `git`/`ls`/`cat` do. That is a PROCESS-WIDE property, which is why this test
//! exercises the disposition rather than any one verb: restoring it covers all
//! ~30 `println!` sites and every verb added later, by construction.
//!
//! Deliberately offline — it uses `--help` so no server is needed and the result
//! is deterministic. Verified to discriminate: against the pre-fix binary this
//! test observes a clean exit 0 (clap swallows its own write error); against the
//! fixed binary, death by SIGPIPE. Gut it and the assert fails.

#![cfg(unix)]

use std::process::{Command, Stdio};

#[test]
fn writing_to_a_closed_pipe_kills_the_process_instead_of_panicking() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_amux-rs"))
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn amux-rs");

    // Close the read end before the child writes. Any write then hits EPIPE, so
    // this does not depend on output volume or on the 64KB pipe buffer.
    drop(child.stdout.take().expect("piped stdout"));

    let out = child.wait_with_output().expect("wait");

    // `ExitStatus::code()` is None when the process was killed by a signal, which
    // is the behaviour we want. Checking the signal explicitly keeps the test from
    // passing on some *other* signal death.
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        out.status.signal(),
        Some(libc::SIGPIPE),
        "expected death by SIGPIPE (default disposition restored), got {:?}; \
         exit code {:?}, stderr: {}",
        out.status.signal(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );

    // The user-visible half of the bug: a Rust panic message on an ordinary pipe.
    assert!(
        out.stderr.is_empty(),
        "EPIPE must be silent, got: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Guards the direction of the fix: restoring SIG_DFL must not make the CLI die
/// when nothing is wrong. Without this, an assert-it-dies test passes just as well
/// against a binary that dies always.
#[test]
fn a_normal_run_still_exits_cleanly() {
    let out = Command::new(env!("CARGO_BIN_EXE_amux-rs"))
        .arg("--help")
        .output()
        .expect("run amux-rs --help");

    assert!(
        out.status.success(),
        "--help should exit 0 when its output is read, got {:?}",
        out.status,
    );
    assert!(!out.stdout.is_empty(), "--help should print usage");
}
