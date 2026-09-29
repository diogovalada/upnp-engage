use crate::{config::Config, platform::shutdown::Shutdown, session::Status};
use anyhow::{bail, Context, Result};
use crossterm::{
    cursor,
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};
use futures_util::StreamExt;
use std::io::{self, Write};

pub struct Ui {
    pub interactive: bool,
    events: Option<EventStream>,
    clipboard: Option<arboard::Clipboard>,
}

impl Ui {
    pub fn new(interactive: bool) -> Result<Self> {
        if interactive {
            terminal::enable_raw_mode().context("Cannot read terminal input")?;
        }
        Ok(Self {
            interactive,
            events: interactive.then(EventStream::new),
            clipboard: None,
        })
    }
    pub fn write(&self, text: &str) -> Result<()> {
        let mut output = io::stdout().lock();
        if self.interactive {
            write!(output, "{}", text.replace('\n', "\r\n"))?;
        } else {
            write!(output, "{text}")?;
        }
        output.flush()?;
        Ok(())
    }
    pub fn line(&self, text: &str) -> Result<()> {
        self.write(&format!("{text}\n"))
    }
    pub async fn key(&mut self, shutdown: &Shutdown) -> Result<Option<KeyEvent>> {
        loop {
            let events = self
                .events
                .as_mut()
                .context("Interactive input requires a terminal")?;
            let event = tokio::select! {
                biased;
                _ = shutdown.wait() => return Ok(None),
                event = events.next() => event.context("Terminal input closed")??,
            };
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                if is_quit_key(key) {
                    shutdown.request();
                    return Ok(None);
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
                    bail!("Input closed");
                }
                return Ok(Some(key));
            }
        }
    }
    async fn answer(
        &mut self,
        label: &str,
        default: &str,
        shutdown: &Shutdown,
    ) -> Result<Option<String>> {
        self.write(&format!(
            "{label}{}: ",
            if default.is_empty() {
                String::new()
            } else {
                format!(" [{default}]")
            }
        ))?;
        let mut answer = String::new();
        loop {
            let Some(key) = self.key(shutdown).await? else {
                return Ok(None);
            };
            match key.code {
                KeyCode::Esc => {
                    self.line("\nCancelled.")?;
                    return Ok(None);
                }
                KeyCode::Enter => {
                    self.line("")?;
                    return Ok(Some(answer));
                }
                KeyCode::Backspace if !answer.is_empty() => {
                    answer.pop();
                    self.write("\x08 \x08")?;
                }
                KeyCode::Char(c) if c.is_ascii() && !c.is_control() && answer.len() < 64 => {
                    answer.push(c);
                    self.write(&c.to_string())?;
                }
                _ => {}
            }
        }
    }
    pub async fn confirm(
        &mut self,
        label: &str,
        default: bool,
        shutdown: &Shutdown,
    ) -> Result<bool> {
        loop {
            let Some(answer) = self
                .answer(label, if default { "Y/n" } else { "y/N" }, shutdown)
                .await?
            else {
                return Ok(false);
            };
            match answer.trim().to_ascii_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => self.line("Enter y or n.")?,
            }
        }
    }
    pub async fn ports(&mut self, current: Config, shutdown: &Shutdown) -> Result<Option<Config>> {
        self.line("\nPress Esc to cancel, or Ctrl+C to quit.")?;
        let device = loop {
            let default = if current.device_port == 0 {
                String::new()
            } else {
                current.device_port.to_string()
            };
            let Some(answer) = self
                .answer("Device port (your application's port)", &default, shutdown)
                .await?
            else {
                return Ok(None);
            };
            match parse_port(&answer, current.device_port, false) {
                Ok(port) => break port,
                Err(error) => self.line(&error.to_string())?,
            }
        };
        let router = loop {
            let default = if current.router_port == 0 {
                format!("{device}, same as device")
            } else {
                current.router_port.to_string()
            };
            let Some(answer) = self
                .answer(
                    "Router port (the port friends connect to)",
                    &default,
                    shutdown,
                )
                .await?
            else {
                return Ok(None);
            };
            match parse_port(&answer, current.router_port, true) {
                Ok(port) => break port,
                Err(error) => self.line(&error.to_string())?,
            }
        };
        Ok(Some(Config {
            device_port: device,
            router_port: router,
        }))
    }
    pub fn show(&self, status: &Status) -> Result<()> {
        if self.interactive {
            execute!(
                io::stdout(),
                terminal::Clear(ClearType::All),
                cursor::MoveTo(0, 0)
            )?;
        }
        self.line("UPnP Engage\n")?;
        if status.busy {
            self.line("Starting port forwarding...")?;
        } else if status.active {
            self.line("Port forwarding active\n")?;
            if let Some(address) = status.share_address() {
                self.line(&format!("Share with friends:\n    {address}\n"))?;
            } else if let Some(address) = status.address {
                self.line(&format!(
                    "Last known address (not current): {address}:{}\n",
                    status.config.external_port()
                ))?;
            } else {
                self.line("External address unavailable. Retrying shortly.\n")?;
            }
            self.line(&format!(
                "Device port: {}   Router port: {}",
                status.config.device_port,
                status.config.external_port()
            ))?;
        } else {
            self.line("Forwarding is stopped.")?;
        }
        if !status.message.is_empty() && !status.busy {
            self.line(&format!("\n{}", status.message))?;
        }
        if self.interactive {
            self.line("\n[C] Copy address   [P] Change ports   [Q] Quit")?;
        } else if status.active {
            self.line("Press Ctrl+C to stop.")?;
        }
        Ok(())
    }
    pub fn copy(&mut self, status: &Status) -> Result<()> {
        let Some(address) = status.share_address() else {
            return self.line("There is no current active address to copy.");
        };
        let result = (|| -> Result<()> {
            if self.clipboard.is_none() {
                self.clipboard = Some(arboard::Clipboard::new()?);
            }
            self.clipboard.as_mut().unwrap().set_text(address.clone())?;
            Ok(())
        })();
        match result {
            Ok(()) => self.line(&format!("Copied {address}")),
            Err(error) => self.line(&format!(
                "Clipboard unavailable ({error}). Select and copy {address} manually."
            )),
        }
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        if self.interactive {
            let _ = terminal::disable_raw_mode();
        }
    }
}

fn is_quit_key(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

fn parse_port(answer: &str, default: u16, router: bool) -> Result<u16> {
    let port = if answer.trim().is_empty() {
        default
    } else {
        answer
            .trim()
            .parse::<u16>()
            .context("Enter a whole port number from 1 to 65535.")?
    };
    if port == 0 && !router {
        bail!("Enter a port number from 1 to 65535.");
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_defaults_keep_auto_router_port_and_validate_device() {
        assert_eq!(parse_port("", 0, true).unwrap(), 0);
        assert_eq!(parse_port("", 80, false).unwrap(), 80);
        assert!(parse_port("", 0, false).is_err());
        assert!(parse_port("65536", 0, true).is_err());
        assert!(parse_port("-2", 0, false).is_err());
    }
    #[test]
    fn raw_control_c_requests_shutdown() {
        assert!(is_quit_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_quit_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
    }
}
