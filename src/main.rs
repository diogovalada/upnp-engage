mod cli;
mod config;
mod gateway;
mod platform;
mod session;
mod ui;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use config::{Config, ConfigFile};
use crossterm::event::KeyCode;
use platform::{launch, shutdown::Shutdown};
use session::{Command, Status};
use std::{env, future, io, process::ExitCode, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};
use ui::Ui;

#[tokio::main]
async fn main() -> ExitCode {
    let args = cli::Args::parse();
    match launch::maybe_launch(&args) {
        Ok(true) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            return ExitCode::FAILURE;
        }
        Ok(false) => {}
    }
    let shutdown = Shutdown::new();
    let result = async {
        shutdown.install()?;
        run(args, shutdown.clone(), None).await
    }
    .await;
    shutdown.finish();
    if let Err(error) = result {
        eprintln!("\n{error:#}");
        if launch::owns_console() && launch::interactive() && !shutdown.is_requested() {
            eprintln!("Press Enter to close.");
            let _ = io::stdin().read_line(&mut String::new());
        }
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn run(
    args: cli::Args,
    shutdown: Arc<Shutdown>,
    router: Option<Box<dyn gateway::Router>>,
) -> Result<()> {
    let interactive = !args.non_interactive && launch::interactive();
    if args.interactive && !interactive {
        bail!("Interactive setup needs a terminal. Open a terminal and run upnp-engage there.");
    }
    let mut ui = Ui::new(interactive)?;
    let user_path = config::user_config_path();
    let mut file = config::discover(
        args.config.as_deref(),
        &env::current_exe()?,
        &env::current_dir()?,
        user_path.as_deref(),
    )?;
    if let Some(issue) = &file.issue {
        if !interactive || args.save_config {
            bail!("{issue}");
        }
        ui.line(issue)?;
        if !ui
            .confirm("Enter settings for this run?", true, &shutdown)
            .await?
        {
            return Ok(());
        }
    } else if file.exists() {
        ui.line(&format!("Loaded {}", file.path.display()))?;
    }
    let mut settings = file.config.overlay(args.device_port, args.router_port);
    let prompted = args.interactive || settings.device_port == 0 || file.issue.is_some();
    if prompted {
        if !interactive {
            bail!("A device port is missing. Set device_port in {} or pass --device-port PORT. Run from a terminal for guided setup.", file.path.display());
        }
        let Some(selected) = ui.ports(settings, &shutdown).await? else {
            return Ok(());
        };
        settings = selected;
    }
    settings.validate()?;
    if shutdown.is_requested() {
        return Ok(());
    }
    if args.save_config {
        file.save(settings, false)?;
        ui.line(&format!("Saved {}", file.path.display()))?;
    } else if prompted {
        offer_save(&mut ui, &mut file, settings, &shutdown).await?;
    }
    if shutdown.is_requested() {
        return Ok(());
    }

    let (commands, receiver) = mpsc::channel(1);
    let (updates, statuses) = watch::channel(Status::default());
    let worker = tokio::spawn(session::worker(receiver, updates, shutdown.clone(), router));
    let result = control(&mut ui, &mut file, settings, &commands, statuses, &shutdown).await;
    shutdown.request();
    drop(commands);
    let cleanup = worker.await.context("Forwarding worker failed")?;
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => Err(anyhow!("{error:#}\nCleanup: {cleanup:#}")),
        (Err(error), _) => Err(error),
        (_, cleanup) => cleanup,
    }
}

async fn offer_save(
    ui: &mut Ui,
    file: &mut ConfigFile,
    settings: Config,
    shutdown: &Shutdown,
) -> Result<()> {
    let label = if file.issue.is_some() {
        "Replace the invalid config"
    } else {
        "Save these settings"
    };
    if !ui
        .confirm(
            &format!("{label} at {}?", file.path.display()),
            !file.exists(),
            shutdown,
        )
        .await?
        || shutdown.is_requested()
    {
        return Ok(());
    }
    if let Err(error) = file.save(settings, file.issue.is_some()) {
        ui.line(&format!("Settings were not saved: {error:#}"))?;
        if !file.explicit && !file.exists() {
            if let Some(path) = config::user_config_path().filter(|path| *path != file.path) {
                if ui
                    .confirm(
                        &format!("Save to {} instead?", path.display()),
                        true,
                        shutdown,
                    )
                    .await?
                    && !shutdown.is_requested()
                {
                    let mut fallback = match ConfigFile::open(path, false) {
                        Ok(file) => file,
                        Err(error) => {
                            ui.line(&format!("Settings were not saved: {error:#}"))?;
                            return Ok(());
                        }
                    };
                    // Never overwrite an existing fallback file without first loading it normally.
                    if fallback.exists() {
                        ui.line(
                            "A config already exists there. Restart to load it before saving.",
                        )?;
                    } else {
                        match fallback.save(settings, false) {
                            Ok(()) => {
                                ui.line(&format!("Saved {}", fallback.path.display()))?;
                                *file = fallback;
                            }
                            Err(error) => {
                                ui.line(&format!("Settings were not saved: {error:#}"))?
                            }
                        }
                    }
                }
            }
        }
    } else {
        ui.line(&format!("Saved {}", file.path.display()))?;
    }
    Ok(())
}

async fn apply(
    commands: &mpsc::Sender<Command>,
    config: Config,
) -> Result<oneshot::Receiver<Result<(), String>>> {
    let (reply, response) = oneshot::channel();
    commands
        .send(Command::Apply(config, reply))
        .await
        .context("Forwarding worker closed")?;
    Ok(response)
}

async fn control(
    ui: &mut Ui,
    file: &mut ConfigFile,
    initial: Config,
    commands: &mpsc::Sender<Command>,
    mut statuses: watch::Receiver<Status>,
    shutdown: &Shutdown,
) -> Result<()> {
    let mut pending = Some(apply(commands, initial).await?);
    let mut save_after = false;
    let mut status = Status::default();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            result = async {
                match pending.as_mut() {
                    Some(response) => response.await,
                    None => future::pending().await,
                }
            } => {
                pending = None;
                let result = result.context("Forwarding worker stopped before replying")?;
                status = statuses.borrow_and_update().clone();
                ui.show(&status)?;
                if let Err(error) = result {
                    if !ui.interactive { bail!("{error}"); }
                } else if save_after {
                    offer_save(ui, file, status.config, shutdown).await?;
                    ui.show(&statuses.borrow().clone())?;
                }
                save_after = false;
            }
            changed = statuses.changed() => {
                changed.context("Forwarding worker stopped unexpectedly")?;
                status = statuses.borrow_and_update().clone();
                ui.show(&status)?;
                if !status.active && !status.busy && pending.is_none() && !ui.interactive { bail!("{}", status.message); }
            }
            key = ui.key(shutdown), if ui.interactive => {
                let Some(key) = key? else { break; };
                match key.code {
                    KeyCode::Char('q' | 'Q') => { shutdown.request(); break; }
                    KeyCode::Char('c' | 'C') => ui.copy(&statuses.borrow().clone())?,
                    KeyCode::Char('p' | 'P') if pending.is_none() => {
                        if let Some(config) = ui.ports(status.config, shutdown).await? {
                            if !shutdown.is_requested() {
                                pending = Some(apply(commands, config).await?);
                                save_after = true;
                            }
                        }
                        ui.show(&statuses.borrow().clone())?;
                    }
                    _ => {},
                }
            }
        }
    }
    ui.line("\nStopping port forwarding...")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An opt-in PTY fixture. It uses the real UI with an in-memory router.
    #[test]
    #[ignore]
    fn interactive_fixture() {
        let Some(log) = env::var_os("UPNP_TEST_LOG").map(std::path::PathBuf::from) else {
            return;
        };
        let path = env::var_os("UPNP_TEST_CONFIG").expect("fixture config path");
        let args = cli::Args::try_parse_from([
            std::ffi::OsString::from("upnp-engage"),
            "--config".into(),
            path,
        ])
        .unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let shutdown = Shutdown::new();
            shutdown.install().unwrap();
            let result = run(
                args,
                shutdown.clone(),
                Some(session::tests::fixture_router(log)),
            )
            .await;
            shutdown.finish();
            result.unwrap();
        });
    }

    #[test]
    #[ignore]
    fn clipboard_fixture() {
        let Some(path) = env::var_os("UPNP_TEST_CLIPBOARD") else {
            return;
        };
        let text = arboard::Clipboard::new().unwrap().get_text().unwrap();
        std::fs::write(path, text).unwrap();
    }
}
