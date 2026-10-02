#[path = "../../packaging/windows_icon.rs"]
mod windows_icon;

fn main() {
    windows_icon::embed("Cross-SystemConnect");
}
