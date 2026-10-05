//! Setup tab back end: the steps that change the computer rather than talk
//! to the AirPods.
//!
//! `setup.sh` is the one implementation of those steps. It is embedded here
//! so a lone binary can run it: the root-only part through `pkexec` (a
//! graphical password prompt), the app-menu entry as the user. Pairing and
//! the status checks live in `airpods-link`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};

const SETUP_SH: &str = include_str!("../../setup.sh");

/// Shown when the password prompt cannot be used.
const TERMINAL_FALLBACK: &str = "Run ./setup.sh from the OpenAirCare folder in a terminal instead.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAction {
    /// Set the Apple vendor ID and restart Bluetooth. `grant_access` also
    /// lets this binary (and user) open Bluetooth sockets.
    Apply { grant_access: bool },
    /// Put `/etc/bluetooth/main.conf` back how it was.
    Undo,
    /// Add OpenAirCare to the app menu.
    DesktopEntry,
}

/// Posted to the UI when an action finishes. The error is worded for the user.
#[derive(Debug, Clone)]
pub struct SetupDone {
    pub action: SetupAction,
    pub result: Result<(), String>,
}

/// Run `action` on its own thread and hand the outcome to `done`.
pub fn spawn(action: SetupAction, done: impl FnOnce(SetupDone) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("openaircare-setup".into())
        .spawn(move || done(SetupDone { action, result: run(action) }));
    if let Err(e) = spawned {
        log::error!("cannot start the setup thread: {e}");
    }
}

fn run(action: SetupAction) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("Setup only runs on Linux.".into());
    }
    let script = write_script().map_err(|e| format!("Could not prepare the setup script: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| format!("Could not find the app file: {e}"))?;
    let mut cmd = match action {
        SetupAction::DesktopEntry => Command::new("bash"),
        _ => {
            let mut c = Command::new("pkexec");
            c.arg("bash");
            c
        }
    };
    cmd.arg(&script);
    match action {
        SetupAction::Apply { grant_access } => {
            cmd.arg("--root-apply");
            if grant_access {
                cmd.arg("--setcap").arg(&exe);
                if let Some(user) = std::env::var_os("USER") {
                    cmd.arg("--add-user").arg(user);
                }
            }
        }
        SetupAction::Undo => {
            cmd.args(["--undo", "--yes"]);
        }
        SetupAction::DesktopEntry => {
            cmd.arg("--desktop-entry").arg(&exe);
        }
    }
    log::info!("setup: running {cmd:?}");
    let out = cmd.output();
    let _ = std::fs::remove_file(&script);
    let privileged = action != SetupAction::DesktopEntry;
    match out {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(explain_failure(&out, privileged)),
        Err(e) if privileged && e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("This desktop has no password prompt (pkexec is not installed). {TERMINAL_FALLBACK}"))
        }
        Err(e) => Err(format!("Could not start setup: {e}")),
    }
}

/// `setup.sh` in a file only this user can write, so root runs what was
/// embedded. `XDG_RUNTIME_DIR` is a per-user 0700 directory.
fn write_script() -> std::io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("openaircare-setup-{}.sh", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o700);
    }
    // A Windows checkout may carry CRLF, which bash rejects.
    opts.open(&path)?.write_all(SETUP_SH.replace("\r\n", "\n").as_bytes())?;
    Ok(path)
}

fn explain_failure(out: &Output, privileged: bool) -> String {
    // pkexec: 126 = the prompt was dismissed, 127 = not authorised or no
    // prompt could be shown.
    match out.status.code() {
        Some(126) if privileged => return "Cancelled: no password was entered. Nothing was changed.".into(),
        Some(127) if privileged => {
            return format!("The password was not accepted, or this desktop cannot show a password prompt. {TERMINAL_FALLBACK}")
        }
        _ => {}
    }
    script_message(&String::from_utf8_lossy(&out.stderr), &String::from_utf8_lossy(&out.stdout))
}

/// What the script said went wrong: its stderr, or failing that the last
/// lines it printed. The `  !!  ` / `  ..  ` markers are for the terminal.
fn script_message(stderr: &str, stdout: &str) -> String {
    let clean = |s: &str| -> Vec<String> {
        s.lines()
            .map(|l| l.trim().trim_start_matches("!!").trim_start_matches("..").trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };
    let err = clean(stderr);
    if !err.is_empty() {
        return err.join("\n");
    }
    let out = clean(stdout);
    if out.is_empty() {
        return "Setup failed without a message.".into();
    }
    out[out.len().saturating_sub(3)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_script_has_the_modes_the_app_calls() {
        for flag in ["--root-apply)", "--setcap)", "--add-user)", "--desktop-entry)", "--undo)", "--yes)"] {
            assert!(SETUP_SH.contains(flag), "setup.sh no longer handles {flag}");
        }
    }

    #[test]
    fn failure_message_prefers_stderr_and_drops_markers() {
        assert_eq!(script_message("  !!  Could not restart Bluetooth.\n", "Configuring Bluetooth.\n"), "Could not restart Bluetooth.");
        assert_eq!(script_message("", "a\nb\n\nc\nd\n"), "b\nc\nd");
        assert_eq!(script_message("", ""), "Setup failed without a message.");
    }
}
