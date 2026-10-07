#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cmd;
mod db;
mod engine;
mod http_api;
mod live;
mod model;
mod platform;
mod tools;
mod ui;
mod update;
mod ytdlp;

use std::io::Write;

fn main() -> eframe::Result<()> {
    let root = tools::install_root();
    let (_, _, _, logs) = tools::ensure_dirs(&root);
    let _ = fastframe_log::Logging::new("mengdownloader", env!("CARGO_PKG_VERSION"))
        .filter("warn,mengdownloader=info")
        .file(logs.join("mengdownloader.log"))
        .panic_log(logs.join("panic.log"))
        .init();

    if std::env::args().any(|arg| arg == "--check") {
        #[cfg(windows)]
        attach_parent_console();
        print_check(&root);
        return Ok(());
    }

    let waker = fastframe_shell::Waker::default();
    let app = ui::MengApp::new(waker.clone());
    fastframe_shell::Shell::new(app, &waker)
        .idle(fastframe_tray::idle)
        .run(ui::open_window)
}

fn print_check(root: &std::path::Path) {
    let paths = tools::ToolPaths::resolve(&model::Settings::default());
    let status = paths.status();
    emit(&format!("root {}", root.display()));
    emit(&format!(
        "yt-dlp {} {}",
        if status.ytdlp_ok { "ok" } else { "missing" },
        status.ytdlp
    ));
    emit(&format!(
        "ffmpeg {} {}",
        if status.ffmpeg_ok { "ok" } else { "missing" },
        status.ffmpeg
    ));
    emit(&format!(
        "ffplay {} {}",
        if status.ffplay_ok { "ok" } else { "missing" },
        status.ffplay
    ));
}

fn emit(line: &str) {
    println!("{line}");
    if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let _ = writeln!(console, "{line}");
    }
}

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
