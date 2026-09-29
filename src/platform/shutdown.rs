use anyhow::Result;
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::watch;

pub struct Shutdown {
    requested: watch::Sender<bool>,
    completed: (Mutex<bool>, Condvar),
}

impl Shutdown {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            requested: watch::channel(false).0,
            completed: (Mutex::new(false), Condvar::new()),
        })
    }
    pub fn request(&self) {
        self.requested.send_replace(true);
    }
    pub fn is_requested(&self) -> bool {
        *self.requested.borrow()
    }
    pub async fn wait(&self) {
        let mut receiver = self.requested.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
    pub fn finish(&self) {
        *self.completed.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.completed.1.notify_all();
    }
    #[cfg(windows)]
    pub fn wait_for_cleanup(&self) {
        let done = self.completed.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.completed.1.wait_timeout_while(
            done,
            std::time::Duration::from_millis(4200),
            |done| !*done,
        );
    }
    pub fn install(self: &Arc<Self>) -> Result<()> {
        #[cfg(windows)]
        super::windows::install(self.clone())?;
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let mut hangup = signal(SignalKind::hangup())?;
            let shutdown = self.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = interrupt.recv() => {},
                    _ = terminate.recv() => {},
                    _ = hangup.recv() => {},
                }
                shutdown.request();
            });
        }
        Ok(())
    }
}
