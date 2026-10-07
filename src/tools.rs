use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::{Settings, ToolStatus};

pub const HTTP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36";

pub const API_PORTS: [u16; 3] = [4000, 41777, 18765];

pub fn install_root() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from(r"I:\mengdownloader");
    };
    let dir = exe.parent().unwrap_or(Path::new(r"I:\mengdownloader")).to_path_buf();
    let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name == "release" || name == "debug" {
        if let Some(target) = dir.parent() {
            if target.file_name().and_then(|s| s.to_str()) == Some("target") {
                if let Some(root) = target.parent() {
                    return root.to_path_buf();
                }
            }
        }
    }
    dir
}

pub fn ensure_dirs(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let data = root.join("data");
    let downloads = root.join("downloads");
    let lives = downloads.join("lives");
    let logs = root.join("logs");
    for dir in [&data, &downloads, &lives, &logs] {
        let _ = std::fs::create_dir_all(dir);
    }
    (data, downloads, lives, logs)
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

fn which(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn tool_candidates(settings_path: &str, file_name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !settings_path.trim().is_empty() {
        out.push(PathBuf::from(settings_path.trim()));
    }
    if let Ok(env_name) = std::env::var(match file_name {
        "yt-dlp.exe" => "YTDLP_PATH",
        "ffmpeg.exe" => "FFMPEG_PATH",
        _ => "",
    }) {
        if !env_name.trim().is_empty() {
            out.push(PathBuf::from(env_name.trim()));
        }
    }
    let root = install_root();
    out.push(root.join("tools").join(file_name));
    out.push(PathBuf::from(r"N:\deep\tools").join(file_name));
    if let Some(found) = which(file_name) {
        out.push(found);
    }
    out
}

pub fn find_tool(settings_path: &str, file_name: &str) -> Option<PathBuf> {
    tool_candidates(settings_path, file_name).into_iter().find(|p| is_file(p))
}

#[derive(Clone, Debug)]
pub struct ToolPaths {
    pub ytdlp: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub ffplay: Option<PathBuf>,
}

impl ToolPaths {
    pub fn resolve(settings: &Settings) -> Self {
        let ytdlp = find_tool(&settings.ytdlp_path, "yt-dlp.exe");
        let ffmpeg = find_tool(&settings.ffmpeg_path, "ffmpeg.exe");
        let ffprobe = ffmpeg.as_ref().and_then(|p| sibling(p, "ffprobe.exe")).filter(|p| is_file(p));
        let ffplay = ffmpeg.as_ref().and_then(|p| sibling(p, "ffplay.exe")).filter(|p| is_file(p))
            .or_else(|| find_tool("", "ffplay.exe"));
        Self { ytdlp, ffmpeg, ffprobe, ffplay }
    }

    pub fn status(&self) -> ToolStatus {
        ToolStatus {
            ytdlp: display(&self.ytdlp),
            ffmpeg: display(&self.ffmpeg),
            ffplay: display(&self.ffplay),
            ytdlp_ok: self.ytdlp.is_some(),
            ffmpeg_ok: self.ffmpeg.is_some(),
            ffplay_ok: self.ffplay.is_some(),
        }
    }
}

fn sibling(path: &Path, name: &str) -> Option<PathBuf> {
    Some(path.parent()?.join(name))
}

fn display(path: &Option<PathBuf>) -> String {
    path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "未找到".into())
}

pub fn hidden(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd;
}

pub fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/F", "/T", "/PID", &pid.to_string()]);
        hidden(&mut cmd);
        let _ = cmd.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
}

pub fn reveal(path: &Path) {
    if !path.exists() {
        return;
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("explorer");
        if path.is_file() {
            cmd.arg(format!("/select,{}", path.display()));
        } else {
            cmd.arg(path);
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = open::that(path);
    }
}

pub fn open_dir(path: &Path) {
    let target = if path.is_file() { path.parent().unwrap_or(path) } else { path };
    if !target.exists() {
        let _ = std::fs::create_dir_all(target);
    }
    let _ = open::that(target);
}

pub fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

pub fn lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("223.5.5.5:53").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    if ip.is_loopback() {
        None
    } else {
        Some(ip.to_string())
    }
}

pub fn stream_headers(platform: &str) -> String {
    let referer = match platform {
        "kuaishou" => "https://live.kuaishou.com/",
        "bilibili" => "https://live.bilibili.com/",
        "huya" => "https://www.huya.com/",
        "douyu" => "https://www.douyu.com/",
        "tiktok" => "https://www.tiktok.com/",
        "xiaohongshu" => "https://www.xiaohongshu.com/",
        _ => "https://live.douyin.com/",
    };
    let origin = referer.trim_end_matches('/');
    format!("User-Agent: {HTTP_UA}\r\nReferer: {referer}\r\nOrigin: {origin}\r\n")
}
