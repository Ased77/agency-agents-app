//! Process helpers for commands that run entirely in the background.
//!
//! Windows GUI applications must opt out of console creation for console
//! subsystem children. Without `CREATE_NO_WINDOW`, short-lived commands such
//! as the catalog's `git` probes briefly open a terminal window.

use std::ffi::OsStr;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Build a standard-library command that must not display a console window.
pub(crate) fn headless_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut command = Command::new(program);
    configure_headless(&mut command);
    command
}

/// Build a Tokio command that must not display a console window.
pub(crate) fn headless_tokio_command<S: AsRef<OsStr>>(program: S) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    configure_headless(command.as_std_mut());
    command
}

fn configure_headless(command: &mut Command) {
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    #[cfg(not(target_os = "windows"))]
    let _ = command;
}
