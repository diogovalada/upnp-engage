use crate::cli::Args;
use anyhow::Result;
use std::io::{self, IsTerminal};

pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub fn owns_console() -> bool {
    #[cfg(windows)]
    {
        let mut processes = [0u32; 2];
        // SAFETY: the buffer holds the declared number of process IDs.
        unsafe { winapi::um::wincon::GetConsoleProcessList(processes.as_mut_ptr(), 2) == 1 }
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("UPNP_ENGAGE_TERMINAL_CHILD").is_some()
    }
}

pub fn maybe_launch(args: &Args) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        linux_launch(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS release binaries use .command so Finder opens them in Terminal.
        // Terminal supplies the TTY; no companion script or AppleScript is needed.
        let _ = args;
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
const TERMINALS: &[(&str, &[&str])] = &[
    ("xdg-terminal-exec", &["--"]),
    ("x-terminal-emulator", &["-e"]),
    ("gnome-terminal", &["--wait", "--"]),
    ("kgx", &["--"]),
    ("konsole", &["--nofork", "-e"]),
    ("xfce4-terminal", &["--disable-server", "-x"]),
    ("xterm", &["-e"]),
];

#[cfg(target_os = "linux")]
fn desktop_candidate(tty: bool, graphical: bool, detached: bool, child: bool, batch: bool) -> bool {
    !tty && graphical && detached && !child && !batch
}

#[cfg(target_os = "linux")]
fn terminal_command(
    program: &str,
    prefix: &[&str],
    executable: &std::path::Path,
    arguments: &[std::ffi::OsString],
) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .args(prefix)
        .arg(executable)
        .args(arguments)
        .env("UPNP_ENGAGE_TERMINAL_CHILD", "1");
    command
}

#[cfg(target_os = "linux")]
fn linux_launch(args: &Args) -> Result<bool> {
    use std::{
        env, fs,
        path::Path,
        process::{Command, Stdio},
        thread,
        time::Duration,
    };
    // A pipe or regular file is redirected input/output, not a desktop launch.
    let detached = [0, 1].iter().all(|fd| {
        fs::read_link(format!("/proc/self/fd/{fd}"))
            .is_ok_and(|path| path == Path::new("/dev/null"))
    });
    if !desktop_candidate(
        interactive(),
        env::var_os("DISPLAY").is_some() || env::var_os("WAYLAND_DISPLAY").is_some(),
        detached,
        env::var_os("UPNP_ENGAGE_TERMINAL_CHILD").is_some(),
        args.non_interactive,
    ) {
        return Ok(false);
    }
    let executable = env::current_exe()?;
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    for (terminal, prefix) in TERMINALS {
        if let Ok(mut child) = terminal_command(terminal, prefix, &executable, &arguments).spawn() {
            thread::sleep(Duration::from_millis(150));
            match child.try_wait()? {
                Some(status) if !status.success() => continue,
                _ => return Ok(true),
            }
        }
    }
    let message = "Could not open a terminal. Run upnp-engage from a terminal window.";
    let _ = Command::new("notify-send")
        .args(["UPnP Engage", message])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    anyhow::bail!(message)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn launch_requires_detached_desktop_and_cannot_recurse() {
        assert!(desktop_candidate(false, true, true, false, false));
        assert!(!desktop_candidate(false, true, false, false, false));
        assert!(!desktop_candidate(false, true, true, true, false));
        assert!(!desktop_candidate(false, true, true, false, true));
        assert!(!desktop_candidate(true, true, true, false, false));
        assert!(!desktop_candidate(false, false, true, false, false));
    }
    #[test]
    fn paths_and_arguments_are_passed_literally() {
        let command = terminal_command(
            "xterm",
            &["-e"],
            std::path::Path::new("/my folder/upnp-engage"),
            &["--config".into(), "/settings/a $file.toml".into()],
        );
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(
            arguments,
            [
                "-e",
                "/my folder/upnp-engage",
                "--config",
                "/settings/a $file.toml"
            ]
        );
    }
}
