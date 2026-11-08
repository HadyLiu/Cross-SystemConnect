//! Wayland: the lock / stay / pause hotkeys through the xdg-desktop-portal
//! GlobalShortcuts interface. Under Wayland an X11 client only sees keys while
//! one of its own windows has focus, so the XInput2 watch in `grab` misses them.
//! The compositor asks the user once to confirm the bindings.

use super::portal::{next_token, request_on, var, BoxErr, DEST, PATH};
use crate::lock::Watch;
use dbus::arg::PropMap;
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";
/// Host apps need an ID with a matching desktop file for the desktop to remember the bindings.
const APP_ID: &str = "io.github.HadyLiu.CrossSystemConnect";
/// The user may take a while in the confirmation dialog.
const BIND_WAIT: Duration = Duration::from_secs(300);

type Binding = (Watch, u32, u32);

static CMD: Mutex<Option<Sender<Vec<Binding>>>> = Mutex::new(None);

/// Bind the currently watched combinations. No-op outside Wayland.
pub fn sync() {
    if !super::wayland_session() {
        return;
    }
    let list: Vec<Binding> = Watch::ALL
        .iter()
        .filter_map(|w| w.combo().map(|(mods, key)| (*w, mods, key)))
        .collect();
    let Ok(mut guard) = CMD.lock() else {
        return;
    };
    if let Some(tx) = guard.as_ref() {
        if tx.send(list.clone()).is_ok() {
            return;
        }
    }
    let (tx, rx) = mpsc::channel();
    let _ = tx.send(list);
    let started = std::thread::Builder::new()
        .name("global-shortcuts".into())
        .spawn(move || {
            if let Err(e) = run(rx) {
                warn!("GlobalShortcuts portal: {e}");
            }
        })
        .is_ok();
    if started {
        *guard = Some(tx);
    }
}

fn run(rx: Receiver<Vec<Binding>>) -> Result<(), BoxErr> {
    let conn = Connection::new_session()?;
    // The registry refuses a connection that already talked to the portal.
    register(&conn);
    let version = {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        conn.with_proxy(DEST, PATH, Duration::from_millis(800))
            .get::<u32>(IFACE, "version")
    };
    if let Err(e) = version {
        info!("GlobalShortcuts portal unavailable ({e}); local hotkeys need an X11 window focused");
        return Ok(());
    }
    // One permission dialog at a time: let the input capture prompt finish first.
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while !super::portal::ready() && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
    }
    let current: Arc<Mutex<String>> = Arc::default();
    let wanted = current.clone();
    let rule = MatchRule::new_signal(IFACE, "Activated");
    conn.add_match(
        rule,
        move |(s, id, _ts, _opts): (dbus::Path, String, u64, PropMap), _, _| {
            if wanted.lock().is_ok_and(|w| *w == *s) {
                if let Some(w) = watch_of(&id) {
                    info!(shortcut = id, "global shortcut");
                    w.hit();
                }
            }
            true
        },
    )?;
    let mut session: Option<dbus::Path<'static>> = None;
    // A refused set is not asked again until it changes; every config save resends it.
    let mut tried: Option<Vec<Binding>> = None;
    loop {
        match rx.recv_timeout(Duration::ZERO) {
            Ok(list) => {
                if tried.as_ref() != Some(&list) && !list.is_empty() {
                    tried = Some(list.clone());
                    // The desktop binds once per session; a new set needs a new session.
                    if let Some(old) = session.take() {
                        close_session(&conn, &old);
                    }
                    let fresh = create_session(&conn)?;
                    if let Ok(mut w) = current.lock() {
                        *w = fresh.to_string();
                    }
                    if let Err(e) = bind(&conn, &fresh, &list) {
                        warn!("binding global shortcuts: {e}");
                    }
                    session = Some(fresh);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        conn.process(Duration::from_millis(200))?;
    }
}

/// Give this host app an ID, so the desktop can show and remember who asks.
fn register(conn: &Connection) {
    if let Err(e) = write_desktop_file() {
        warn!("desktop file for {APP_ID}: {e}");
    }
    let proxy = conn.with_proxy(DEST, PATH, Duration::from_secs(3));
    let mut last = None;
    // A freshly written desktop file can take a moment to be noticed.
    for _ in 0..6 {
        let opts = PropMap::new();
        match proxy.method_call::<(), _, _, _>(REGISTRY, "Register", (APP_ID, opts)) {
            Ok(()) => {
                info!("registered with the portal as {APP_ID}");
                return;
            }
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some(e) = last {
        warn!("portal registry: {e}");
    }
}

fn write_desktop_file() -> std::io::Result<()> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share")))
        .ok_or_else(|| std::io::Error::other("no HOME"))?;
    let dir = data.join("applications");
    let exe = std::env::current_exe()?;
    let bin = exe.parent().unwrap_or(std::path::Path::new("/"));
    let ui = bin.join("Cross-SystemConnect");
    let icon = bin.join("app.png");
    let mut text = format!(
        "[Desktop Entry]\nType=Application\nName=Cross-SystemConnect\n\
         Exec=\"{}\"\nTerminal=false\nNoDisplay=true\n",
        ui.display()
    );
    if icon.is_file() {
        text.push_str(&format!("Icon={}\n", icon.display()));
    }
    let path = dir.join(format!("{APP_ID}.desktop"));
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(path, text)
}

fn create_session(conn: &Connection) -> Result<dbus::Path<'static>, BoxErr> {
    let token = next_token();
    let mut opts = PropMap::new();
    opts.insert("handle_token".into(), var(token.clone()));
    opts.insert("session_handle_token".into(), var(next_token()));
    let results = request_on(
        conn,
        IFACE,
        "CreateSession",
        &token,
        (opts,),
        Duration::from_secs(10),
    )?;
    let handle = dbus::arg::prop_cast::<String>(&results, "session_handle")
        .cloned()
        .ok_or("CreateSession gave no session handle")?;
    Ok(dbus::Path::new(handle)?)
}

fn close_session(conn: &Connection, session: &dbus::Path<'static>) {
    let proxy = conn.with_proxy(DEST, session.clone(), Duration::from_secs(3));
    if let Err(e) = proxy.method_call::<(), _, _, _>("org.freedesktop.portal.Session", "Close", ())
    {
        warn!("closing the shortcuts session: {e}");
    }
}

fn bind(conn: &Connection, session: &dbus::Path<'static>, list: &[Binding]) -> Result<(), BoxErr> {
    let zh = chinese();
    let shortcuts: Vec<(String, PropMap)> = list
        .iter()
        .filter_map(|&(w, mods, key)| {
            let trigger = trigger(mods, key)?;
            let mut m = PropMap::new();
            m.insert("description".into(), var(description(w, zh).to_string()));
            m.insert("preferred_trigger".into(), var(trigger));
            Some((id_of(w).to_string(), m))
        })
        .collect();
    let token = next_token();
    let mut opts = PropMap::new();
    opts.insert("handle_token".into(), var(token.clone()));
    let results = request_on(
        conn,
        IFACE,
        "BindShortcuts",
        &token,
        (session.clone(), shortcuts, "", opts),
        BIND_WAIT,
    )?;
    let mut got = Vec::new();
    if let Some(items) = results.get("shortcuts").and_then(|v| v.0.as_iter()) {
        for item in items {
            let Some(mut it) = item.as_iter() else {
                continue;
            };
            let id = it.next().and_then(|v| v.as_str().map(str::to_string));
            let desc = it.next().and_then(|props| {
                let mut kv = props.as_iter()?;
                while let (Some(k), Some(v)) = (kv.next(), kv.next()) {
                    if k.as_str() == Some("trigger_description") {
                        return v.as_iter()?.next()?.as_str().map(str::to_string);
                    }
                }
                None
            });
            got.push(format!(
                "{}={}",
                id.unwrap_or_default(),
                desc.unwrap_or_default()
            ));
        }
    }
    info!(shortcuts = ?got, "global shortcuts bound");
    Ok(())
}

fn id_of(w: Watch) -> &'static str {
    match w {
        Watch::Lock => "lock-screen",
        Watch::Stay => "lock-to-screen",
        Watch::Pause => "emergency-pause",
    }
}

fn watch_of(id: &str) -> Option<Watch> {
    Watch::ALL.into_iter().find(|w| id_of(*w) == id)
}

fn description(w: Watch, zh: bool) -> &'static str {
    match (w, zh) {
        (Watch::Lock, true) => "锁定屏幕",
        (Watch::Lock, false) => "Lock screen",
        (Watch::Stay, true) => "锁定在当前屏幕",
        (Watch::Stay, false) => "Lock to screen",
        (Watch::Pause, true) => "紧急暂停",
        (Watch::Pause, false) => "Emergency pause",
    }
}

fn chinese() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("zh"))
}

/// XDG shortcut trigger such as `CTRL+SHIFT+l` (modifier bits from `keymap::modifier_bit`).
fn trigger(mods: u32, key: u32) -> Option<String> {
    let mut out = String::new();
    for (bit, name) in [(1, "CTRL"), (4, "ALT"), (2, "SHIFT"), (8, "LOGO")] {
        if mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&keysym(key)?);
    Some(out)
}

/// xkb keysym name for a HID usage.
fn keysym(hid: u32) -> Option<String> {
    let name = match hid {
        0x04..=0x1D => return Some(char::from(b'a' + (hid - 0x04) as u8).to_string()),
        0x1E..=0x26 => return Some(char::from(b'1' + (hid - 0x1E) as u8).to_string()),
        0x27 => "0",
        0x3A..=0x45 => return Some(format!("F{}", hid - 0x3A + 1)),
        0x28 => "Return",
        0x29 => "Escape",
        0x2A => "BackSpace",
        0x2B => "Tab",
        0x2C => "space",
        0x2D => "minus",
        0x2E => "equal",
        0x2F => "bracketleft",
        0x30 => "bracketright",
        0x31 => "backslash",
        0x33 => "semicolon",
        0x34 => "apostrophe",
        0x35 => "grave",
        0x36 => "comma",
        0x37 => "period",
        0x38 => "slash",
        0x39 => "Caps_Lock",
        0x46 => "Print",
        0x47 => "Scroll_Lock",
        0x48 => "Pause",
        0x49 => "Insert",
        0x4A => "Home",
        0x4B => "Page_Up",
        0x4C => "Delete",
        0x4D => "End",
        0x4E => "Page_Down",
        0x4F => "Right",
        0x50 => "Left",
        0x51 => "Down",
        0x52 => "Up",
        _ => return None,
    };
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::trigger;

    #[test]
    fn triggers_follow_the_xdg_format() {
        assert_eq!(trigger(1 | 2, 0x0F).as_deref(), Some("CTRL+SHIFT+l"));
        assert_eq!(trigger(1 | 4, 0x38).as_deref(), Some("CTRL+ALT+slash"));
        assert_eq!(trigger(8, 0x45).as_deref(), Some("LOGO+F12"));
        assert_eq!(trigger(1, 0xE5), None);
    }
}
