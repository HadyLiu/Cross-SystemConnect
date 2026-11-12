#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // Elevated service mode: runs before (and instead of) the async core.
    if let Some(code) = csc_core::service_cli() {
        std::process::exit(code);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(csc_core::run())
}
