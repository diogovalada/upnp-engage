use super::shutdown::Shutdown;
use anyhow::{bail, Result};
use std::sync::{Arc, OnceLock};
use winapi::shared::minwindef::{BOOL, DWORD, FALSE, TRUE};
use winapi::um::consoleapi::SetConsoleCtrlHandler;
use winapi::um::wincon::{
    CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
};

static SHUTDOWN: OnceLock<Arc<Shutdown>> = OnceLock::new();

unsafe extern "system" fn handler(event: DWORD) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            if let Some(shutdown) = SHUTDOWN.get() {
                shutdown.request();
                // Windows terminates the process when a close callback returns.
                // Keep this thread alive while the normal runtime performs cleanup.
                if matches!(
                    event,
                    CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
                ) {
                    shutdown.wait_for_cleanup();
                }
                TRUE
            } else {
                FALSE
            }
        }
        _ => FALSE,
    }
}

pub fn install(shutdown: Arc<Shutdown>) -> Result<()> {
    if SHUTDOWN.set(shutdown).is_err() {
        bail!("Console shutdown handling is already installed");
    }
    // SAFETY: handler has the required ABI and its shared state lives for the process lifetime.
    if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
