use crate::{
    config::Config,
    gateway::{Mapping, NetworkRouter, Router},
    platform::shutdown::Shutdown,
};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::future::join_all;
use igd::PortMappingProtocol::{self, TCP, UDP};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{self, Instant};

const RENEWAL: Duration = Duration::from_secs(3000);
const REFRESH: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub active: bool,
    pub busy: bool,
    pub config: Config,
    pub address: Option<Ipv4Addr>,
    pub fresh: bool,
    pub message: String,
}

impl Status {
    pub fn share_address(&self) -> Option<String> {
        if self.active && self.fresh && !self.busy {
            self.address
                .map(|ip| format!("{ip}:{}", self.config.external_port()))
        } else {
            None
        }
    }
}

#[derive(Clone)]
struct Lease {
    protocol: PortMappingProtocol,
    port: u16,
    destination: SocketAddrV4,
}

pub struct Session {
    router: Box<dyn Router>,
    description: String,
    leases: Vec<Lease>,
    shutdown: Arc<Shutdown>,
    active: bool,
}

impl Session {
    pub fn new(router: Box<dyn Router>, shutdown: Arc<Shutdown>) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            router,
            description: format!("UPnP Engage {}-{nonce:x}", std::process::id()),
            leases: Vec::new(),
            shutdown,
            active: false,
        }
    }
    fn check_running(&self) -> Result<()> {
        if self.shutdown.is_requested() {
            bail!("Stopping");
        }
        Ok(())
    }
    pub async fn start(&mut self, config: Config) -> Result<()> {
        config.validate()?;
        self.stop().await?;
        let result = self.create(config).await;
        if let Err(error) = result {
            return match self.stop().await {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow!("{error:#}; cleanup also failed: {cleanup:#}")),
            };
        }
        self.active = true;
        Ok(())
    }
    async fn create(&mut self, config: Config) -> Result<()> {
        for protocol in [TCP, UDP] {
            self.check_running()?;
            let port = config.external_port();
            if self.router.mapping(protocol, port).await?.is_some() {
                bail!("Router port {port} ({protocol}) already has a forwarding rule. Choose another router port.");
            }
            self.check_running()?;
            let destination = SocketAddrV4::new(self.router.local_ip(), config.device_port);
            // Record the attempt BEFORE sending: a timed-out request may still have reached the router.
            self.leases.push(Lease {
                protocol,
                port,
                destination,
            });
            self.router
                .add(protocol, port, destination, &self.description)
                .await
                .with_context(|| format!("Could not forward {protocol} port {port}"))?;
            self.check_running()?;
            let mapping = self.router.mapping(protocol, port).await?;
            if !self.owns(&self.leases[self.leases.len() - 1], mapping.as_ref()) {
                bail!("The router did not confirm ownership of the {protocol} mapping");
            }
        }
        self.check_running()
    }
    fn owns(&self, lease: &Lease, mapping: Option<&Mapping>) -> bool {
        mapping.is_some_and(|mapping| {
            mapping.destination == lease.destination && mapping.description == self.description
        })
    }
    pub async fn renew(&mut self) -> Result<()> {
        for lease in &self.leases {
            self.check_running()?;
            let current = self.router.mapping(lease.protocol, lease.port).await?;
            if !self.owns(lease, current.as_ref()) {
                bail!(
                    "The {} mapping was removed or changed; forwarding has stopped",
                    lease.protocol
                );
            }
            self.check_running()?;
            self.router
                .add(
                    lease.protocol,
                    lease.port,
                    lease.destination,
                    &self.description,
                )
                .await?;
        }
        Ok(())
    }
    pub async fn stop(&mut self) -> Result<()> {
        self.active = false;
        // Both protocols are removed concurrently to fit within the Windows close budget.
        // Only this owner issues add/renew operations, so none can race these removals.
        let results = join_all(self.leases.iter().map(|lease| async {
            let mapping = self.router.mapping(lease.protocol, lease.port).await?;
            if mapping.is_none() {
                return Ok(());
            }
            if !self.owns(lease, mapping.as_ref()) {
                bail!(
                    "{} port {} belongs to another mapping; left it untouched",
                    lease.protocol,
                    lease.port
                );
            }
            self.router.remove(lease.protocol, lease.port).await
        }))
        .await;
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for (lease, result) in self.leases.drain(..).zip(results) {
            if let Err(error) = result {
                errors.push(format!("{} port {}: {error:#}", lease.protocol, lease.port));
                remaining.push(lease);
            }
        }
        self.leases = remaining;
        if !errors.is_empty() {
            bail!("Some mappings could not be removed: {}. Their finite leases may remain until expiry.", errors.join("; "));
        }
        Ok(())
    }
}

pub enum Command {
    Apply(Config, oneshot::Sender<Result<(), String>>),
}

pub async fn worker(
    mut commands: mpsc::Receiver<Command>,
    updates: watch::Sender<Status>,
    shutdown: Arc<Shutdown>,
    router: Option<Box<dyn Router>>,
) -> Result<()> {
    let mut session = router.map(|router| Session::new(router, shutdown.clone()));
    let mut status = Status::default();
    let mut renewal = Instant::now() + RENEWAL;
    let mut refresh = Instant::now() + REFRESH;
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait() => break,
            command = commands.recv() => {
                let Some(Command::Apply(config, reply)) = command else { break; };
                status.busy = true; status.message = "Starting port forwarding...".into();
                updates.send_replace(status.clone());
                let result = async {
                    if session.is_none() {
                        let router = tokio::select! {
                            _ = shutdown.wait() => return Err(anyhow!("Stopping")),
                            router = NetworkRouter::discover() => router?,
                        };
                        session = Some(Session::new(Box::new(router), shutdown.clone()));
                    }
                    session.as_mut().unwrap().start(config).await
                }.await;
                status.busy = false; status.config = config; status.address = None; status.fresh = false;
                status.active = result.is_ok();
                status.message = result.as_ref().err().map(|error| format!("{error:#}")).unwrap_or_default();
                if status.active && !shutdown.is_requested() {
                    update_address(session.as_ref().unwrap(), &mut status).await;
                }
                renewal = Instant::now() + RENEWAL; refresh = Instant::now() + REFRESH;
                updates.send_replace(status.clone());
                let _ = reply.send(result.map_err(|error| format!("{error:#}")));
            }
            _ = time::sleep_until(renewal), if status.active => {
                let owner = session.as_mut().unwrap();
                if let Err(error) = owner.renew().await {
                    status.active = false; status.fresh = false;
                    status.message = format!("Forwarding stopped: {error:#}");
                    if let Err(cleanup) = owner.stop().await { status.message.push_str(&format!("\n{cleanup:#}")); }
                    updates.send_replace(status.clone());
                }
                renewal = Instant::now() + RENEWAL;
            }
            _ = time::sleep_until(refresh), if status.active => {
                let previous = status.clone();
                update_address(session.as_ref().unwrap(), &mut status).await;
                if status != previous { updates.send_replace(status.clone()); }
                refresh = Instant::now() + REFRESH;
            }
        }
    }
    if let Some(mut session) = session {
        session.stop().await?;
    }
    Ok(())
}

async fn update_address(session: &Session, status: &mut Status) {
    match session.router.external_ip().await {
        Ok(address) => {
            status.address = Some(address);
            status.fresh = true;
            status.message.clear();
        }
        Err(error) => {
            status.fresh = false;
            status.message = format!("Could not refresh the external address: {error:#}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeState {
        mappings: Vec<(PortMappingProtocol, u16, Mapping)>,
        fail_udp: bool,
        fail_remove: bool,
        adds: usize,
        remove_calls: usize,
        stop_on_add: Option<Arc<Shutdown>>,
        fail_after_add: bool,
        log: Option<std::path::PathBuf>,
        address: Option<Ipv4Addr>,
        fail_external: bool,
    }
    struct Fake(Arc<Mutex<FakeState>>);
    #[async_trait]
    impl Router for Fake {
        fn local_ip(&self) -> Ipv4Addr {
            Ipv4Addr::new(192, 168, 1, 2)
        }
        async fn external_ip(&self) -> Result<Ipv4Addr> {
            let state = self.0.lock().unwrap();
            if state.fail_external {
                bail!("address lookup failed");
            }
            Ok(state.address.unwrap_or(Ipv4Addr::new(203, 0, 113, 1)))
        }
        async fn mapping(
            &self,
            protocol: PortMappingProtocol,
            port: u16,
        ) -> Result<Option<Mapping>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .mappings
                .iter()
                .find(|(p, n, _)| *p == protocol && *n == port)
                .map(|(_, _, m)| m.clone()))
        }
        async fn add(
            &self,
            protocol: PortMappingProtocol,
            port: u16,
            destination: SocketAddrV4,
            description: &str,
        ) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            if state.fail_udp && protocol == UDP {
                bail!("UDP refused");
            }
            state.adds += 1;
            if let Some(path) = &state.log {
                append_log(path, &format!("add {protocol} {port}"));
            }
            state
                .mappings
                .retain(|(p, n, _)| *p != protocol || *n != port);
            state.mappings.push((
                protocol,
                port,
                Mapping {
                    destination,
                    description: description.into(),
                },
            ));
            if let Some(stop) = &state.stop_on_add {
                stop.request();
            }
            if state.fail_after_add {
                bail!("reply lost after mapping created");
            }
            Ok(())
        }
        async fn remove(&self, protocol: PortMappingProtocol, port: u16) -> Result<()> {
            let mut state = self.0.lock().unwrap();
            state.remove_calls += 1;
            if state.fail_remove {
                bail!("router unavailable");
            }
            if let Some(path) = &state.log {
                append_log(path, &format!("remove {protocol} {port}"));
            }
            state
                .mappings
                .retain(|(p, n, _)| *p != protocol || *n != port);
            Ok(())
        }
    }
    fn setup() -> (Session, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState::default()));
        (
            Session::new(Box::new(Fake(state.clone())), Shutdown::new()),
            state,
        )
    }
    fn settings(port: u16) -> Config {
        Config {
            device_port: 8080,
            router_port: port,
        }
    }

    fn append_log(path: &std::path::Path, text: &str) {
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{text}"
        )
        .unwrap();
    }

    pub fn fixture_router(log: std::path::PathBuf) -> Box<dyn Router> {
        Box::new(Fake(Arc::new(Mutex::new(FakeState {
            log: Some(log),
            ..Default::default()
        }))))
    }

    /// Runs only in a separate test process; never part of the shipped executable.
    #[test]
    #[ignore]
    fn signal_fixture() {
        let Some(path) = std::env::var_os("UPNP_TEST_LOG").map(std::path::PathBuf::from) else {
            return;
        };
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let shutdown = Shutdown::new();
            shutdown.install().unwrap();
            let mut session = Session::new(fixture_router(path.clone()), shutdown.clone());
            session.start(settings(9000)).await.unwrap();
            #[cfg(windows)]
            let ready = unsafe { winapi::um::wincon::GetConsoleWindow() as usize }.to_string();
            #[cfg(not(windows))]
            let ready = "ready";
            std::fs::write(path.with_extension("ready"), ready).unwrap();
            shutdown.wait().await;
            session.stop().await.unwrap();
            append_log(&path, "cleaned");
            shutdown.finish();
        });
    }

    #[cfg(unix)]
    #[test]
    fn os_signals_remove_mappings_in_a_separate_process() {
        use std::{
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for signal in ["-INT", "-TERM", "-HUP"] {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("signal.log");
            let mut child = ChildGuard(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "session::tests::signal_fixture",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("UPNP_TEST_LOG", &log)
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while !log.with_extension("ready").exists() {
                assert!(Instant::now() < deadline, "child never became ready");
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "child exited before signal"
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert!(Command::new("kill")
                .args([signal, &child.0.id().to_string()])
                .status()
                .unwrap()
                .success());
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline, "shutdown hung");
                thread::sleep(Duration::from_millis(10));
            }
            let contents = std::fs::read_to_string(log).unwrap();
            assert!(contents.contains("remove TCP 9000"));
            assert!(contents.contains("remove UDP 9000"));
            assert!(contents.contains("cleaned"));
        }
    }

    #[tokio::test]
    async fn rolls_back_partial_start_and_uncertain_adds() {
        for fail_after_add in [false, true] {
            let (mut session, state) = setup();
            state.lock().unwrap().fail_udp = true;
            state.lock().unwrap().fail_after_add = fail_after_add;
            assert!(session.start(settings(9000)).await.is_err());
            assert!(state.lock().unwrap().mappings.is_empty());
        }
    }
    #[tokio::test]
    async fn changes_ports_and_cleanup_is_idempotent() {
        let (mut session, state) = setup();
        session.start(settings(9000)).await.unwrap();
        session.start(settings(9001)).await.unwrap();
        assert_eq!(state.lock().unwrap().mappings.len(), 2);
        assert!(state
            .lock()
            .unwrap()
            .mappings
            .iter()
            .all(|(_, port, _)| *port == 9001));
        session.stop().await.unwrap();
        session.stop().await.unwrap();
        assert!(state.lock().unwrap().mappings.is_empty());
        assert_eq!(state.lock().unwrap().remove_calls, 4);
    }
    #[tokio::test]
    async fn never_takes_over_or_deletes_a_foreign_mapping() {
        let (mut session, state) = setup();
        let foreign = Mapping {
            destination: "192.168.1.8:80".parse().unwrap(),
            description: "Other app".into(),
        };
        state
            .lock()
            .unwrap()
            .mappings
            .push((TCP, 9000, foreign.clone()));
        assert!(session.start(settings(9000)).await.is_err());
        assert_eq!(state.lock().unwrap().adds, 0);
        state.lock().unwrap().mappings.clear();
        session.start(settings(9000)).await.unwrap();
        state.lock().unwrap().mappings[0].2 = foreign;
        assert!(session.renew().await.is_err());
        assert!(session.stop().await.is_err());
        assert_eq!(state.lock().unwrap().mappings.len(), 1);
        assert_eq!(state.lock().unwrap().mappings[0].2.description, "Other app");
    }
    #[tokio::test]
    async fn shutdown_during_add_prevents_further_adds_and_removes_result() {
        let (mut session, state) = setup();
        state.lock().unwrap().stop_on_add = Some(session.shutdown.clone());
        assert!(session.start(settings(9000)).await.is_err());
        assert_eq!(state.lock().unwrap().adds, 1);
        assert!(state.lock().unwrap().mappings.is_empty());
    }
    #[tokio::test]
    async fn cleanup_failure_blocks_new_mappings_and_can_be_retried() {
        let (mut session, state) = setup();
        session.start(settings(9000)).await.unwrap();
        state.lock().unwrap().fail_remove = true;
        assert!(session.start(settings(9001)).await.is_err());
        assert_eq!(state.lock().unwrap().adds, 2);
        state.lock().unwrap().fail_remove = false;
        session.stop().await.unwrap();
        assert!(state.lock().unwrap().mappings.is_empty());
    }
    #[test]
    fn copying_requires_active_fresh_address_and_uses_router_port() {
        let mut status = Status {
            active: true,
            config: settings(9000),
            address: Some(Ipv4Addr::new(203, 0, 113, 4)),
            fresh: true,
            ..Default::default()
        };
        assert_eq!(status.share_address().unwrap(), "203.0.113.4:9000");
        status.fresh = false;
        assert!(status.share_address().is_none());
        status.fresh = true;
        status.active = false;
        assert!(status.share_address().is_none());
        status.active = true;
        status.busy = true;
        assert!(status.share_address().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn worker_refreshes_addresses_marks_them_stale_and_cleans_up_after_renewal_failure() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let shutdown = Shutdown::new();
        let (commands, receiver) = mpsc::channel(1);
        let (updates, mut statuses) = watch::channel(Status::default());
        let task = tokio::spawn(worker(
            receiver,
            updates,
            shutdown.clone(),
            Some(Box::new(Fake(state.clone()))),
        ));
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::Apply(settings(9000), reply))
            .await
            .unwrap();
        result.await.unwrap().unwrap();
        assert!(statuses.borrow_and_update().active);
        state.lock().unwrap().address = Some(Ipv4Addr::new(203, 0, 113, 99));
        time::advance(REFRESH).await;
        statuses.changed().await.unwrap();
        assert_eq!(
            statuses.borrow_and_update().share_address().unwrap(),
            "203.0.113.99:9000"
        );
        state.lock().unwrap().fail_external = true;
        time::advance(REFRESH).await;
        statuses.changed().await.unwrap();
        assert!(statuses.borrow_and_update().share_address().is_none());
        state.lock().unwrap().fail_udp = true;
        time::advance(RENEWAL).await;
        statuses.changed().await.unwrap();
        assert!(!statuses.borrow_and_update().active);
        assert!(state.lock().unwrap().mappings.is_empty());
        shutdown.request();
        task.await.unwrap().unwrap();
    }
}
