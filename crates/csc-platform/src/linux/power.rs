//! Notice logind before this computer sleeps, and again when it wakes.

use crate::PowerEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

static KEEP_AWAKE: AtomicBool = AtomicBool::new(true);

pub fn set_keep_awake(on: bool) {
    KEEP_AWAKE.store(on, Ordering::Relaxed);
}

/// Reset the screen saver / idle timer on this computer.
pub fn note_user_activity() {
    let _ = std::process::Command::new("xset")
        .args(["s", "reset"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Ok(conn) = dbus::blocking::Connection::new_session() {
        let proxy = conn.with_proxy(
            "org.freedesktop.ScreenSaver",
            "/org/freedesktop/ScreenSaver",
            Duration::from_millis(200),
        );
        let _: Result<(), dbus::Error> =
            proxy.method_call("org.freedesktop.ScreenSaver", "SimulateUserActivity", ());
    }
}

pub fn spawn() -> Receiver<PowerEvent> {
    let (tx, rx) = mpsc::channel();
    let locks = tx.clone();
    std::thread::Builder::new()
        .name("csc-power".into())
        .spawn(move || {
            if let Err(e) = listen(tx) {
                tracing::debug!("power watch stopped: {e}");
            }
        })
        .ok();
    std::thread::Builder::new()
        .name("csc-lock".into())
        .spawn(move || {
            if let Err(e) = watch_screensaver(locks) {
                tracing::debug!("lock watch stopped: {e}");
            }
        })
        .ok();
    rx
}

fn watch_screensaver(tx: Sender<PowerEvent>) -> Result<(), Box<dyn std::error::Error>> {
    let conn = dbus::blocking::Connection::new_session()?;
    // GNOME (Ubuntu) emits the lock only on org.gnome.ScreenSaver; KDE, Deepin
    // and others on org.freedesktop.ScreenSaver. Listen to both; a duplicate
    // Locked is harmless.
    for iface in ["org.freedesktop.ScreenSaver", "org.gnome.ScreenSaver"] {
        let send = MatchSend(tx.clone());
        conn.add_match(
            dbus::message::MatchRule::new_signal(iface, "ActiveChanged"),
            move |(active,): (bool,), _: &dbus::blocking::Connection, _: &dbus::Message| {
                let ev = if active {
                    PowerEvent::Locked
                } else {
                    PowerEvent::Unlocked
                };
                let _ = send.0.send(ev);
                true
            },
        )?;
    }
    loop {
        conn.process(Duration::from_secs(60))?;
    }
}

fn listen(tx: Sender<PowerEvent>) -> Result<(), Box<dyn std::error::Error>> {
    let conn = dbus::blocking::Connection::new_system()?;
    let proxy = conn.with_proxy(
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        Duration::from_secs(2),
    );
    // Delay sleep long enough to tell the other computers and release keys.
    let (_inhibit,): (dbus::arg::OwnedFd,) = proxy.method_call(
        "org.freedesktop.login1.Manager",
        "Inhibit",
        (
            "sleep",
            "Cross-SystemConnect",
            "release remote keys",
            "delay",
        ),
    )?;
    let sleep = MatchSend(tx.clone());
    conn.add_match(
        dbus::message::MatchRule::new_signal("org.freedesktop.login1.Manager", "PrepareForSleep"),
        move |(sleeping,): (bool,), _: &dbus::blocking::Connection, _: &dbus::Message| {
            let ev = if sleeping {
                PowerEvent::Suspending
            } else {
                PowerEvent::Resumed
            };
            let _ = sleep.0.send(ev);
            true
        },
    )?;
    let down = MatchSend(tx);
    conn.add_match(
        dbus::message::MatchRule::new_signal(
            "org.freedesktop.login1.Manager",
            "PrepareForShutdown",
        ),
        move |(shutting,): (bool,), _: &dbus::blocking::Connection, _: &dbus::Message| {
            if shutting {
                let _ = down.0.send(PowerEvent::ShuttingDown);
            }
            true
        },
    )?;
    let mut idle: Option<dbus::arg::OwnedFd> = None;
    loop {
        let want = KEEP_AWAKE.load(Ordering::Relaxed);
        if want && idle.is_none() {
            let held: Result<(dbus::arg::OwnedFd,), dbus::Error> = proxy.method_call(
                "org.freedesktop.login1.Manager",
                "Inhibit",
                (
                    "idle",
                    "Cross-SystemConnect",
                    "someone is using a connected computer",
                    "block",
                ),
            );
            idle = held.ok().map(|(fd,)| fd);
        } else if !want {
            idle = None;
        }
        conn.process(Duration::from_secs(5))?;
    }
}

struct MatchSend(Sender<PowerEvent>);
