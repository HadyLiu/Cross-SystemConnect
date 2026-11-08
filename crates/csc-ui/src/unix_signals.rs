//! Catch shell/dock Quit (SIGTERM / SIGINT) while the window is minimized or idle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Install handlers and a watcher thread that exits even if the egui loop is asleep.
pub fn install() {
    unsafe extern "C" fn on_signal(_: libc::c_int) {
        QUIT_REQUESTED.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_signal as *const () as libc::sighandler_t,
        );
    }

    let _ = std::thread::Builder::new()
        .name("csc-sigquit".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_millis(50));
            if QUIT_REQUESTED.swap(false, Ordering::SeqCst) {
                tracing::info!("SIGTERM/SIGINT: quitting");
                crate::singleton::release();
                crate::ipc_client::force_quit_now();
            }
        });
}
