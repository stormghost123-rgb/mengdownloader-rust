use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Waiting,
    Parsing,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Parsing => "parsing",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "等待",
            Self::Parsing => "解析",
            Self::Downloading => "下载中",
            Self::Paused => "已暂停",
            Self::Completed => "完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "parsing" => Self::Parsing,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Waiting,
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Waiting | Self::Parsing | Self::Downloading | Self::Paused | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomStatus {
    Idle,
    Queued,
    Resolving,
    Recording,
    Stopping,
    Error,
    Done,
}

impl RoomStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Resolving => "resolving",
            Self::Recording => "recording",
            Self::Stopping => "stopping",
            Self::Error => "error",
            Self::Done => "done",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "空闲",
            Self::Queued => "排队",
            Self::Resolving => "解析中",
            Self::Recording => "录制中",
            Self::Stopping => "停止中",
            Self::Error => "出错",
            Self::Done => "已结束",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "resolving" => Self::Resolving,
            "recording" => Self::Recording,
            "stopping" => Self::Stopping,
            "error" => Self::Error,
            "done" => Self::Done,
            _ => Self::Idle,
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Queued | Self::Resolving | Self::Recording | Self::Stopping)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingStatus {
    Recording,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

impl RecordingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Recording => "录制中",
            Self::Completed => "完成",
            Self::Partial => "不完整",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "partial" => Self::Partial,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Recording,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub url: String,
    pub title: String,
    pub platform: String,
    pub platform_label: String,
    pub author: String,
    pub thumbnail: String,
    pub duration: i64,
    pub quality: String,
    pub container: String,
    pub format_id: String,
    pub ext: String,
    pub filesize: i64,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub speed: f64,
    pub eta: i64,
    pub percent: f64,
    pub status: TaskStatus,
    pub error: String,
    pub output_path: String,
    pub save_dir: String,
    pub progress_text: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: i64,
    pub completed_at: i64,
}

impl Task {
    pub fn blank(url: String) -> Self {
        let now = now_ms();
        Self {
            id: new_id(),
            url,
            title: String::new(),
            platform: "generic".into(),
            platform_label: String::new(),
            author: String::new(),
            thumbnail: String::new(),
            duration: 0,
            quality: "best".into(),
            container: "mp4".into(),
            format_id: "best".into(),
            ext: "mp4".into(),
            filesize: 0,
            downloaded_bytes: 0,
            total_bytes: 0,
            speed: 0.0,
            eta: 0,
            percent: 0.0,
            status: TaskStatus::Waiting,
            error: String::new(),
            output_path: String::new(),
            save_dir: String::new(),
            progress_text: String::new(),
            created_at: now,
            updated_at: now,
            started_at: 0,
            completed_at: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FormatOption {
    pub id: String,
    pub label: String,
    pub height: i64,
    pub ext: String,
    pub filesize: i64,
    pub note: String,
    pub has_audio: bool,
}

#[derive(Clone, Debug)]
pub struct ParseResult {
    pub url: String,
    pub title: String,
    pub platform: String,
    pub platform_label: String,
    pub author: String,
    pub thumbnail: String,
    pub duration: i64,
    pub formats: Vec<FormatOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseView {
    Idle,
    Busy,
    Ready,
    Failed(String),
}

impl Default for ParseView {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Debug)]
pub struct LiveRoom {
    pub id: String,
    pub url: String,
    pub quality: String,
    pub format: String,
    pub platform: String,
    pub platform_label: String,
    pub anchor_name: String,
    pub title: String,
    pub thumbnail: String,
    pub status: RoomStatus,
    pub message: String,
    pub error: String,
    pub output_path: String,
    pub record_url: String,
    pub alt_url: String,
    pub current_recording_id: String,
    pub filesize: i64,
    pub speed: f64,
    pub started_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub live_on: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct LiveRecording {
    pub id: String,
    pub room_id: String,
    pub url: String,
    pub platform: String,
    pub platform_label: String,
    pub anchor_name: String,
    pub title: String,
    pub quality: String,
    pub format: String,
    pub status: RecordingStatus,
    pub error: String,
    pub output_path: String,
    pub filesize: i64,
    pub duration_sec: i64,
    pub started_at: i64,
    pub ended_at: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub download_dir: String,
    pub max_concurrency: i64,
    pub download_threads: i64,
    pub default_quality: String,
    pub default_format: String,
    pub max_speed: i64,
    pub timeout_sec: i64,
    pub max_retries: i64,
    pub theme: String,
    pub cookie_file: String,
    pub live_dir: String,
    pub live_format: String,
    pub live_quality: String,
    pub live_max_concurrent: i64,
    pub close_to_tray: bool,
    pub ytdlp_path: String,
    pub ffmpeg_path: String,
}

impl Settings {
    pub fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("downloadDir", self.download_dir.clone()),
            ("maxConcurrency", self.max_concurrency.to_string()),
            ("downloadThreads", self.download_threads.to_string()),
            ("defaultQuality", self.default_quality.clone()),
            ("defaultFormat", self.default_format.clone()),
            ("maxSpeed", self.max_speed.to_string()),
            ("timeoutSec", self.timeout_sec.to_string()),
            ("maxRetries", self.max_retries.to_string()),
            ("theme", self.theme.clone()),
            ("cookieFile", self.cookie_file.clone()),
            ("liveDir", self.live_dir.clone()),
            ("liveFormat", self.live_format.clone()),
            ("liveQuality", self.live_quality.clone()),
            ("liveMaxConcurrent", self.live_max_concurrent.to_string()),
            ("closeToTray", self.close_to_tray.to_string()),
            ("ytdlpPath", self.ytdlp_path.clone()),
            ("ffmpegPath", self.ffmpeg_path.clone()),
        ]
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub today_tasks: i64,
    pub completed_today: i64,
    pub downloading: i64,
    pub failed_today: i64,
    pub total_completed: i64,
    pub total_bytes: i64,
    pub total_tasks: i64,
    pub success_rate: f64,
    pub by_platform: Vec<PlatformStat>,
}

#[derive(Clone, Debug)]
pub struct PlatformStat {
    pub platform: String,
    pub label: String,
    pub count: i64,
    pub bytes: i64,
}

#[derive(Clone, Debug, Default)]
pub struct ToolStatus {
    pub ytdlp: String,
    pub ffmpeg: String,
    pub ffplay: String,
    pub ytdlp_ok: bool,
    pub ffmpeg_ok: bool,
    pub ffplay_ok: bool,
}

#[derive(Clone, Debug)]
pub struct ResolvedStream {
    pub is_live: bool,
    pub offline: bool,
    pub record_url: String,
    pub alt_url: String,
    pub anchor_name: String,
    pub title: String,
    pub platform: String,
    pub platform_label: String,
    pub error: String,
    pub source: String,
}

impl ResolvedStream {
    pub fn empty(platform: &str, label: &str) -> Self {
        Self {
            is_live: false,
            offline: false,
            record_url: String::new(),
            alt_url: String::new(),
            anchor_name: String::new(),
            title: String::new(),
            platform: platform.into(),
            platform_label: label.into(),
            error: String::new(),
            source: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Failed,
}

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub phase: UpdatePhase,
    pub message: String,
    pub version: String,
    pub notes: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub banner_dismissed: bool,
}

impl Default for UpdateInfo {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            message: String::new(),
            version: String::new(),
            notes: String::new(),
            asset_url: String::new(),
            asset_size: 0,
            banner_dismissed: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub ready: bool,
    pub boot_error: String,
    pub settings: Settings,
    pub tasks: Vec<Task>,
    pub rooms: Vec<LiveRoom>,
    pub recordings: Vec<LiveRecording>,
    pub parse: ParseView,
    pub parsed: Option<ParseResult>,
    pub notice: String,
    pub notice_error: bool,
    pub stats: Stats,
    pub tools: ToolStatus,
    pub lan_urls: Vec<String>,
    pub api_port: u16,
    pub busy: bool,
    pub update: UpdateInfo,
    pub request_quit: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: String::new(),
            max_concurrency: 3,
            download_threads: 8,
            default_quality: "best".into(),
            default_format: "mp4".into(),
            max_speed: 0,
            timeout_sec: 30,
            max_retries: 3,
            theme: "system".into(),
            cookie_file: String::new(),
            live_dir: String::new(),
            live_format: "mkv".into(),
            live_quality: "OD".into(),
            live_max_concurrent: 12,
            close_to_tray: true,
            ytdlp_path: String::new(),
            ffmpeg_path: String::new(),
        }
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            ready: false,
            boot_error: String::new(),
            settings: Settings::default(),
            tasks: Vec::new(),
            rooms: Vec::new(),
            recordings: Vec::new(),
            parse: ParseView::Idle,
            parsed: None,
            notice: String::new(),
            notice_error: false,
            stats: Stats::default(),
            tools: ToolStatus::default(),
            lan_urls: Vec::new(),
            api_port: 0,
            busy: false,
            update: UpdateInfo::default(),
            request_quit: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestResult {
    pub kind: String,
    pub message: String,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    format!("{:x}{:x}", now_ms(), N.fetch_add(1, Ordering::Relaxed))
}

pub fn local_day(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < 4 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn human_speed(n: f64) -> String {
    if n <= 0.0 {
        "—".into()
    } else {
        format!("{}/s", human_bytes(n as u64))
    }
}

pub fn human_eta(sec: i64) -> String {
    if sec <= 0 {
        return "—".into();
    }
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn human_duration(sec: i64) -> String {
    if sec <= 0 {
        return "—".into();
    }
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if h > 0 {
        format!("{h}小时{m:02}分{s:02}秒")
    } else if m > 0 {
        format!("{m}分{s:02}秒")
    } else {
        format!("{s}秒")
    }
}

pub fn safe_name(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if "\\/:*?\"<>|".contains(ch) || ch.is_control() {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim().trim_matches('.').to_string();
    let clipped: String = trimmed.chars().take(80).collect();
    if clipped.is_empty() { "live".into() } else { clipped }
}

pub fn clock(ms: i64) -> String {
    if ms <= 0 {
        return "—".into();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}
