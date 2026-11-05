use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hivetop::config;
use hivetop::dbus::{Command, Service, Shared};
use hivetop::notify;
use hivetop::roots::Roots;
use hivetop::sampler::Sampler;
use hivetop::{BUS_NAME, OBJECT_PATH};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("hivetopd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let handler = on_signal as extern "C" fn(libc::c_int);
    unsafe {
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }

    let roots = Roots::from_env();
    let history_path = config::data_dir().join("history.db");
    let mut sampler = Sampler::new(roots.clone(), config::config_path(), history_path.clone());
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = mpsc::channel();
    let service = Service::new(
        shared.clone(),
        tx,
        history_path,
        roots.proc.clone(),
        roots.uid,
    );
    let conn = zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()?;
    let iface = conn.object_server().interface::<_, Service>(OBJECT_PATH)?;

    let lock = || shared.lock().unwrap_or_else(|e| e.into_inner());
    let mut waiters: Vec<mpsc::Sender<()>> = Vec::new();
    while !STOP.load(Ordering::SeqCst) {
        let active = lock().is_active();
        let result = sampler.tick(active || !waiters.is_empty());
        let summary = serde_json::to_string(&result.snapshot.summary())?;
        {
            let mut sh = lock();
            sh.snapshot = result.snapshot;
            sh.targets = result.targets;
            sh.docker_socket = sampler.config.docker.socket.clone();
        }
        for w in waiters.drain(..) {
            let _ = w.send(());
        }
        if let Err(e) = zbus::block_on(Service::updated(iface.signal_emitter(), &summary)) {
            eprintln!("hivetopd: signal failed: {e}");
        }
        for n in &result.notices {
            notify::send(&conn, n);
        }
        let deadline = Instant::now() + sampler.interval(active);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || STOP.load(Ordering::SeqCst) {
                break;
            }
            match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
                Ok(Command::Tick(reply)) => {
                    waiters.push(reply);
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    sampler.shutdown();
    Ok(())
}
