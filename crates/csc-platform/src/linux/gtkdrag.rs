//! Wayland drag source (架构设计 §4.6).
//!
//! A Wayland `wl_data_device.start_drag` needs a button serial on our own
//! surface, and GNOME will not place a normal window under the pointer.
//! This prototype opens a tiny XWayland popup on the pointer, presses the
//! left button with uinput (not the remote-desktop portal), and starts a GTK
//! drag of `text/uri-list` placeholders. Mutter turns that Xdnd drag into a
//! Wayland drop, so Files and the desktop copy the placeholders. The copy's
//! directory is the drop location. The popup moves aside once the drag
//! starts, so the window under the pointer receives the drop.
//!
//! The icon is the theme's file or folder (`text-x-generic` / `folder`).

use super::uinput;
use super::xdnd::{self, Top};
use crate::vdrag::{self, DragDesc, DragEvent};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};

static CANCEL: AtomicU64 = AtomicU64::new(0);

pub fn cancel(id: u64) {
    CANCEL.store(id, Ordering::Release);
}

fn drag_dir(id: u64) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir);
    base.join("csc-drag").join(id.to_string())
}

fn icon_name(tops: &[Top]) -> &'static str {
    if tops.iter().all(|t| t.is_dir) {
        "folder"
    } else {
        "text-x-generic"
    }
}

struct Job {
    id: u64,
    dir: PathBuf,
    tops: Vec<Top>,
    ready: Sender<bool>,
    events: Sender<DragEvent>,
}

/// GTK has to stay on the thread that called `gtk::init`.
fn sender() -> &'static Sender<Job> {
    static TX: OnceLock<Sender<Job>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("csc-gtkdrag".into())
            .spawn(move || worker(rx))
            .expect("gtk drag thread");
        tx
    })
}

fn worker(rx: Receiver<Job>) {
    unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    if gtk::init().is_err() {
        warn!("drag: gtk init failed");
        while let Ok(job) = rx.recv() {
            let _ = job.ready.send(false);
            let _ = std::fs::remove_dir_all(&job.dir);
        }
        return;
    }
    while let Ok(job) = rx.recv() {
        let ev = match run(job.id, &job.dir, &job.tops, &job.ready) {
            Ok(ev) => ev,
            Err(e) => {
                warn!("drag {}: {e}", job.id);
                let _ = job.ready.send(false);
                DragEvent::Cancelled {
                    id: job.id,
                    reason: "error",
                }
            }
        };
        let _ = job.events.send(ev);
        let _ = std::fs::remove_dir_all(&job.dir);
    }
}

pub fn start(desc: DragDesc, events: Sender<DragEvent>) -> bool {
    let dir = drag_dir(desc.id);
    let tops = match xdnd::make_placeholders(&dir, desc.id, &desc.items) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return false,
        Err(e) => {
            warn!("drag placeholders in {}: {e}", dir.display());
            return false;
        }
    };
    CANCEL.store(0, Ordering::Release);
    vdrag::arm_follow();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    if sender()
        .send(Job {
            id: desc.id,
            dir: dir.clone(),
            tops,
            ready: ready_tx,
            events,
        })
        .is_err()
    {
        let _ = std::fs::remove_dir_all(&dir);
        return false;
    }
    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap_or(false)
}

fn run(
    id: u64,
    dir: &std::path::Path,
    tops: &[Top],
    ready: &Sender<bool>,
) -> Result<DragEvent, String> {
    info!(
        "drag {id}: system drag of {} item(s) from {}",
        tops.len(),
        dir.display()
    );

    let window = gtk::Window::new(gtk::WindowType::Popup);
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_accept_focus(false);
    window.set_focus_on_map(false);
    window.set_skip_taskbar_hint(true);
    window.set_skip_pager_hint(true);
    window.set_keep_above(true);
    window.set_opacity(0.02);
    window.set_default_size(48, 48);
    if let Some(screen) = gtk::prelude::GtkWindowExt::screen(&window) {
        if let Some(visual) = screen.rgba_visual() {
            window.set_visual(Some(&visual));
        }
    }
    window.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK);
    let (x, y) = vdrag::pointed();
    window.move_(x.saturating_sub(24), y.saturating_sub(24));
    window.show_all();
    for _ in 0..8 {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    let uris: Vec<String> = tops.iter().map(|t| xdnd::file_uri(&t.path)).collect();
    let icon = icon_name(tops);
    let started = Rc::new(Cell::new(false));
    let failed = Rc::new(Cell::new(false));
    let ended = Rc::new(Cell::new(false));
    let context: Rc<RefCell<Option<gdk::DragContext>>> = Rc::new(RefCell::new(None));

    {
        let uris = uris.clone();
        window.connect_drag_data_get(move |_, _, data, _, _| {
            let refs: Vec<&str> = uris.iter().map(String::as_str).collect();
            data.set_uris(&refs);
        });
    }
    {
        let started = started.clone();
        let context = context.clone();
        let window_for_press = window.clone();
        window.connect_button_press_event(move |_, ev| {
            begin_drag(&window_for_press, icon, &started, &context, Some(&*ev));
            glib::Propagation::Stop
        });
    }
    {
        let failed = failed.clone();
        window.connect_drag_failed(move |_, _, result| {
            if !matches!(result, gtk::DragResult::Success) {
                failed.set(true);
            }
            glib::Propagation::Stop
        });
    }
    {
        let ended = ended.clone();
        window.connect_drag_end(move |_, _| {
            ended.set(true);
            gtk::main_quit();
        });
    }

    let begin = Instant::now();
    {
        let started = started.clone();
        let context = context.clone();
        let window = window.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let cancel = CANCEL.load(Ordering::Acquire) == id || vdrag::follow_cancelled();
            if cancel {
                if let Some(ctx) = context.borrow().as_ref() {
                    ctx.drag_cancel();
                } else {
                    gtk::main_quit();
                }
                return glib::ControlFlow::Break;
            }
            if !started.get() && begin.elapsed() > Duration::from_millis(200) {
                begin_drag(&window, icon, &started, &context, None);
            }
            if !started.get() && begin.elapsed() > Duration::from_secs(2) {
                gtk::main_quit();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    let _ = ready.send(true);
    if vdrag::follow_cancelled() || vdrag::released_early() {
        return Ok(DragEvent::Cancelled {
            id,
            reason: "cancelled",
        });
    }
    if uinput::left_down() {
        super::inject_x_button(1, false);
        std::thread::sleep(Duration::from_millis(20));
    }
    if vdrag::follow_cancelled() || vdrag::released_early() {
        return Ok(DragEvent::Cancelled {
            id,
            reason: "cancelled",
        });
    }
    super::inject_x_button(1, true);
    gtk::main();

    if !started.get() || !ended.get() {
        info!("drag {id}: did not become a system drag");
        return Ok(DragEvent::Cancelled {
            id,
            reason: "cancelled",
        });
    }
    if failed.get() || CANCEL.load(Ordering::Acquire) == id || vdrag::follow_cancelled() {
        return Ok(DragEvent::Cancelled {
            id,
            reason: "no target",
        });
    }
    let since = SystemTime::now() - Duration::from_secs(60);
    Ok(xdnd::resolve_copies(id, tops, since))
}

fn begin_drag(
    window: &gtk::Window,
    icon: &str,
    started: &Cell<bool>,
    context: &RefCell<Option<gdk::DragContext>>,
    event: Option<&gdk::Event>,
) {
    if started.get() {
        return;
    }
    let list = gtk::TargetList::new(&[gtk::TargetEntry::new(
        "text/uri-list",
        gtk::TargetFlags::OTHER_APP,
        0,
    )]);
    let Some(ctx) =
        window.drag_begin_with_coordinates(&list, gdk::DragAction::COPY, 1, event, -1, -1)
    else {
        return;
    };
    ctx.drag_set_icon_name(icon, 0, 0);
    // The drop target is whatever is under the pointer, not this popup.
    window.move_(-200, -200);
    context.replace(Some(ctx));
    started.set(true);
}
