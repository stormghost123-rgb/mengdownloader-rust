use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;

use serde_json::Value;

use crate::cmd::{CancelFlag, Cmd};
use crate::model::{FormatOption, ParseResult, Settings, Task};
use crate::platform::detect_platform;
use crate::tools::{self, hidden};

#[derive(Clone, Debug)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    pub speed: f64,
    pub eta: i64,
}

pub fn parse_progress_line(line: &str) -> Option<Progress> {
    let index = line.find("PROG|")?;
    let parts: Vec<&str> = line[index..].split('|').collect();
    if parts.len() < 6 {
        return None;
    }
    let downloaded = num(parts[1]);
    let total = num(parts[2]).max(num(parts[3]));
    Some(Progress {
        downloaded,
        total,
        speed: num(parts[4]) as f64,
        eta: num(parts[5]) as i64,
    })
}

fn num(raw: &str) -> u64 {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("NA") || raw.eq_ignore_ascii_case("None") {
        return 0;
    }
    raw.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64).unwrap_or(0)
}

pub fn format_selector(format_id: &str, container: &str) -> String {
    let clean = "best[format_id!*=download_addr][format_note!*=watermark]";
    let id = format_id.trim();
    if id.is_empty() || id == "best" {
        return format!("{clean}/bestvideo+bestaudio/best");
    }
    if let Some(height) = id.strip_prefix('h').and_then(|n| n.parse::<u32>().ok()) {
        return format!(
            "bestvideo[height<={height}][ext={container}]+bestaudio/bestvideo[height<={height}]+bestaudio/{clean}/best"
        );
    }
    let base = id.split('-').next().unwrap_or(id);
    format!("{id}/{id}+bestaudio/{base}-0/{base}/{clean}/best")
}

pub fn parse(bin: &Path, url: &str, timeout_sec: u64) -> Result<ParseResult, String> {
    let mut cmd = Command::new(bin);
    cmd.args([
        "-J",
        "--no-playlist",
        "--no-warnings",
        "--socket-timeout",
        &timeout_sec.max(5).to_string(),
        url,
    ]);
    hidden(&mut cmd);
    cmd.env("PYTHONUTF8", "1");
    cmd.env("PYTHONIOENCODING", "utf-8");
    let output = cmd.output().map_err(|e| format!("无法启动 yt-dlp：{e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(brief_error(&err));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(text.trim()).map_err(|e| format!("解析结果不是 JSON：{e}"))?;
    Ok(parse_info(&value, url))
}

fn parse_info(value: &Value, url: &str) -> ParseResult {
    let title = value.get("title").and_then(|v| v.as_str()).unwrap_or(url).to_string();
    let author = ["uploader", "creator", "channel", "uploader_id"]
        .iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string();
    let thumbnail = value.get("thumbnail").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let duration = value.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
    let page = value.get("webpage_url").and_then(|v| v.as_str()).unwrap_or(url);
    let platform = detect_platform(page);
    let mut formats = Vec::new();
    formats.push(FormatOption {
        id: "best".into(),
        label: "最佳画质".into(),
        height: 0,
        ext: "mp4".into(),
        filesize: json_size(value),
        note: String::new(),
        has_audio: true,
    });
    if let Some(list) = value.get("formats").and_then(|v| v.as_array()) {
        let mut seen: Vec<(i64, FormatOption)> = Vec::new();
        for item in list {
            let vcodec = item.get("vcodec").and_then(|v| v.as_str()).unwrap_or("none");
            if vcodec == "none" || vcodec.is_empty() {
                continue;
            }
            let note = item.get("format_note").and_then(|v| v.as_str()).unwrap_or("");
            if note.to_ascii_lowercase().contains("storyboard") {
                continue;
            }
            let id = item.get("format_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            let height = item.get("height").and_then(|v| v.as_i64()).unwrap_or(0);
            let ext = item.get("ext").and_then(|v| v.as_str()).unwrap_or("mp4").to_string();
            let filesize = json_size(item);
            let acodec = item.get("acodec").and_then(|v| v.as_str()).unwrap_or("none");
            let label = if height > 0 {
                format!("{height}p {ext}")
            } else if !note.is_empty() {
                format!("{note} {ext}")
            } else {
                format!("{id} {ext}")
            };
            let option = FormatOption {
                id,
                label,
                height,
                ext,
                filesize,
                note: note.to_string(),
                has_audio: acodec != "none" && !acodec.is_empty(),
            };
            if let Some(existing) = seen.iter_mut().find(|(h, _)| *h == height && height > 0) {
                if option.filesize > existing.1.filesize {
                    *existing = (height, option);
                }
            } else {
                seen.push((height, option));
            }
        }
        seen.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, option) in seen.into_iter().take(12) {
            formats.push(option);
        }
    }
    ParseResult {
        url: url.to_string(),
        title,
        platform: platform.id,
        platform_label: platform.label,
        author,
        thumbnail,
        duration,
        formats,
    }
}

fn json_size(value: &Value) -> i64 {
    value
        .get("filesize")
        .or_else(|| value.get("filesize_approx"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

pub fn stream_url(bin: &Path, url: &str) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(["-g", "--no-playlist", "--no-warnings", "-f", "best", url]);
    hidden(&mut cmd);
    cmd.env("PYTHONUTF8", "1");
    let output = cmd.output().map_err(|e| format!("无法启动 yt-dlp：{e}"))?;
    if !output.status.success() {
        return Err(brief_error(&String::from_utf8_lossy(&output.stderr)));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(|s| s.to_string())
        .ok_or_else(|| "yt-dlp 没有返回直播流地址".into())
}

pub fn download(
    tx: Sender<Cmd>,
    task: Task,
    settings: Settings,
    ytdlp: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    flag: Arc<CancelFlag>,
) {
    let id = task.id.clone();
    if flag.cancelled() {
        let _ = tx.send(Cmd::JobExit {
            id,
            code: -1,
            path: String::new(),
            error: String::new(),
            cancelled: true,
        });
        return;
    }
    let Some(bin) = ytdlp else {
        let _ = tx.send(Cmd::JobExit {
            id,
            code: 1,
            path: String::new(),
            error: "未找到 yt-dlp。把 yt-dlp.exe 放到 tools 目录，或沿用 N:\\deep\\tools。".into(),
            cancelled: false,
        });
        return;
    };
    let save_dir = if task.save_dir.trim().is_empty() {
        PathBuf::from(&settings.download_dir)
    } else {
        PathBuf::from(&task.save_dir)
    };
    if let Err(err) = std::fs::create_dir_all(&save_dir) {
        let _ = tx.send(Cmd::JobExit {
            id,
            code: 1,
            path: String::new(),
            error: format!("无法创建下载目录：{err}"),
            cancelled: false,
        });
        return;
    }
    let container = if task.container.is_empty() { "mp4".to_string() } else { task.container.clone() };
    let selector = format_selector(&task.format_id, &container);
    let dir = save_dir.display().to_string().replace('\\', "/");
    let template = format!("{dir}/%(uploader,creator,channel,id)s_%(title).80B [%(id)s].%(ext)s");
    let mut args = vec![
        "--no-playlist".into(),
        "--newline".into(),
        "--continue".into(),
        "--no-mtime".into(),
        "--retries".into(),
        settings.max_retries.to_string(),
        "--fragment-retries".into(),
        settings.max_retries.to_string(),
        "-N".into(),
        settings.download_threads.clamp(1, 16).to_string(),
        "--socket-timeout".into(),
        settings.timeout_sec.clamp(5, 300).to_string(),
        "--progress-delta".into(),
        "1".into(),
        "--progress-template".into(),
        "download:PROG|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s".into(),
        "--print".into(),
        "after_move:%(filepath)s".into(),
        "-f".into(),
        selector,
        "--merge-output-format".into(),
        container.clone(),
        "--remux-video".into(),
        container,
        "-o".into(),
        template,
    ];
    if settings.max_speed > 0 {
        args.push("--limit-rate".into());
        args.push(settings.max_speed.to_string());
    }
    if let Some(ffmpeg) = ffmpeg.as_ref().and_then(|p| p.parent()) {
        args.push("--ffmpeg-location".into());
        args.push(ffmpeg.display().to_string());
    }
    if !settings.cookie_file.trim().is_empty() && Path::new(settings.cookie_file.trim()).is_file() {
        args.push("--cookies".into());
        args.push(settings.cookie_file.clone());
    }
    args.push(task.url.clone());

    let mut cmd = Command::new(&bin);
    cmd.args(&args);
    hidden(&mut cmd);
    cmd.env("PYTHONUTF8", "1");
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = tx.send(Cmd::JobExit {
                id,
                code: -1,
                path: String::new(),
                error: format!("无法启动 yt-dlp：{err}"),
                cancelled: false,
            });
            return;
        }
    };
    flag.set_pid(child.id());
    if flag.cancelled() {
        tools::kill_pid(child.id());
    }
    let stderr = child.stderr.take();
    let stderr_text = Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(stderr) = stderr {
        let bucket = Arc::clone(&stderr_text);
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut acc = String::new();
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if acc.len() < 20_000 {
                    acc.push_str(&line);
                    acc.push('\n');
                }
            }
            if let Ok(mut guard) = bucket.lock() {
                *guard = acc;
            }
        });
    }
    let mut final_path = String::new();
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(progress) = parse_progress_line(&line) {
                let _ = tx.send(Cmd::JobProgress {
                    id: id.clone(),
                    downloaded: progress.downloaded,
                    total: progress.total,
                    speed: progress.speed,
                    eta: progress.eta,
                });
            } else if looks_like_path(&line) {
                final_path = line.trim().trim_matches('"').to_string();
            }
        }
    }
    let status = child.wait();
    let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
    let log = stderr_text.lock().map(|g| g.clone()).unwrap_or_default();
    if final_path.is_empty() {
        final_path = path_from_log(&log);
    }
    let cancelled = flag.cancelled();
    let error = if code == 0 || cancelled { String::new() } else { brief_error(&log) };
    let _ = tx.send(Cmd::JobExit { id, code, path: final_path, error, cancelled });
}

fn looks_like_path(line: &str) -> bool {
    let line = line.trim().trim_matches('"');
    if line.contains("PROG|") || line.is_empty() {
        return false;
    }
    line.contains(":\\") || line.contains(":/") || line.starts_with('/') || line.starts_with("\\\\")
}

pub fn path_from_log(text: &str) -> String {
    for line in text.lines() {
        for marker in ["Destination:", "Merging formats into "] {
            if let Some(rest) = line.split(marker).nth(1) {
                return rest.trim().trim_matches('"').trim_end_matches('"').to_string();
            }
        }
        if let Some(idx) = line.find(" has already been downloaded") {
            let head = line[..idx].trim();
            let path = head.rsplit("] ").next().unwrap_or(head).trim();
            if looks_like_path(path) {
                return path.to_string();
            }
        }
    }
    String::new()
}

pub fn brief_error(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("WARNING"))
        .unwrap_or("下载失败");
    let lower = line.to_ascii_lowercase();
    let mapped = if lower.contains("unsupported url") {
        "不支持的链接"
    } else if lower.contains("sign in") || lower.contains("login") || lower.contains("cookies") {
        "需要登录或 Cookie"
    } else if lower.contains("private video") || lower.contains("not available") {
        "视频不可用，可能是私密或已删除"
    } else if lower.contains("403") {
        "访问被拒绝"
    } else if lower.contains("404") {
        "没有找到视频"
    } else if lower.contains("unable to download") {
        "下载失败"
    } else {
        line
    };
    let mut out: String = mapped.chars().take(240).collect();
    if mapped.chars().count() > 240 {
        out.push('…');
    }
    if out.is_empty() { "下载失败".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_line_ignores_na_and_keeps_the_estimate() {
        let progress = parse_progress_line("PROG|1024|NA|4096|2048|12").unwrap();
        assert_eq!(progress.downloaded, 1024);
        assert_eq!(progress.total, 4096);
        assert_eq!(progress.speed, 2048.0);
        assert_eq!(progress.eta, 12);
    }

    #[test]
    fn destination_line_is_the_output_path() {
        let log = "[download] Destination: I:\\mengdownloader\\downloads\\demo.mp4\n";
        assert_eq!(path_from_log(log), r"I:\mengdownloader\downloads\demo.mp4");
    }
}
