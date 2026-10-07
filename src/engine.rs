use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fastframe_shell::Waker;
use rusqlite::Connection;

use crate::cmd::{CancelFlag, Cmd, IngestKind};
use crate::db;
use crate::http_api;
use crate::live;
use crate::model::{
    human_bytes, new_id, now_ms, IngestResult, LiveRecording, LiveRoom, ParseResult, ParseView, RecordingStatus,
    RoomStatus, Settings, Snapshot, Task, TaskStatus, UpdateInfo, UpdatePhase,
};
use crate::platform;
use crate::tools::{self, ToolPaths};
use crate::update::{self, UpdateReport};
use crate::ytdlp;

enum JobKind {
    Download,
    Live,
}

struct Job {
    flag: Arc<CancelFlag>,
    kind: JobKind,
}

pub fn start(waker: Waker) -> (Sender<Cmd>, Arc<Mutex<Snapshot>>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let snap = Arc::new(Mutex::new(Snapshot::default()));
    let tx_worker = tx.clone();
    let snap_worker = Arc::clone(&snap);
    let handle = thread::Builder::new()
        .name("meng-engine".into())
        .spawn(move || run(rx, tx_worker, snap_worker, waker))
        .expect("engine thread");
    (tx, snap, handle)
}

fn run(rx: Receiver<Cmd>, tx: Sender<Cmd>, snap: Arc<Mutex<Snapshot>>, waker: Waker) {
    let root = tools::install_root();
    let (data, downloads, lives, _) = tools::ensure_dirs(&root);
    let mut defaults = Settings::default();
    defaults.download_dir = downloads.display().to_string();
    defaults.live_dir = lives.display().to_string();
    update::cleanup_previous();
    let db = match db::open(&data.join("meng.sqlite")) {
        Ok(db) => db,
        Err(err) => {
            let mut guard = snap.lock().unwrap_or_else(|e| e.into_inner());
            guard.ready = true;
            guard.boot_error = format!("数据库没有打开：{err}");
            drop(guard);
            waker.wake();
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    Cmd::Ingest { reply, .. } => {
                        let _ = reply.send(Err("数据库没有打开".into()));
                    }
                    Cmd::Shutdown => break,
                    _ => {}
                }
            }
            return;
        }
    };
    if let Err(err) = db::seed_settings(&db, &defaults) {
        log::warn!("settings seed: {err}");
    }
    let snap_boot = Arc::clone(&snap);
    let waker_boot = waker.clone();
    let mut supervisor = match Supervisor::load(db, defaults, rx, tx, snap, waker) {
        Ok(supervisor) => supervisor,
        Err(err) => {
            log::error!("load: {err}");
            let mut guard = snap_boot.lock().unwrap_or_else(|e| e.into_inner());
            guard.ready = true;
            guard.boot_error = format!("启动失败：{err}");
            drop(guard);
            waker_boot.wake();
            return;
        }
    };
    supervisor.publish();
    supervisor.begin_update_check(false);
    while !supervisor.stop {
        match supervisor.rx.recv_timeout(Duration::from_millis(400)) {
            Ok(cmd) => supervisor.handle(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => supervisor.flush(),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for job in supervisor.jobs.values() {
        job.flag.cancel();
        tools::kill_pid(job.flag.pid());
    }
}

struct Supervisor {
    db: Connection,
    snap: Arc<Mutex<Snapshot>>,
    tx: Sender<Cmd>,
    rx: Receiver<Cmd>,
    waker: Waker,
    settings: Settings,
    tasks: Vec<Task>,
    rooms: Vec<LiveRoom>,
    recordings: Vec<LiveRecording>,
    parse: ParseView,
    parsed: Option<ParseResult>,
    parsing_url: String,
    notice: String,
    notice_error: bool,
    jobs: HashMap<String, Job>,
    tools: ToolPaths,
    api_port: u16,
    lan_urls: Vec<String>,
    dirty: bool,
    last_flush: Instant,
    stop: bool,
    update: UpdateInfo,
    update_manual: bool,
    exit_requested: bool,
}

impl Supervisor {
    fn load(
        db: Connection,
        defaults: Settings,
        rx: Receiver<Cmd>,
        tx: Sender<Cmd>,
        snap: Arc<Mutex<Snapshot>>,
        waker: Waker,
    ) -> Result<Self, String> {
        let _ = db::pause_interrupted(&db);
        let settings = db::load_settings(&db, &defaults)?;
        let _ = std::fs::create_dir_all(&settings.download_dir);
        let _ = std::fs::create_dir_all(&settings.live_dir);
        let mut tasks = db::load_tasks(&db)?;
        for task in &mut tasks {
            if matches!(task.status, TaskStatus::Downloading | TaskStatus::Parsing) {
                task.status = TaskStatus::Paused;
                task.speed = 0.0;
                task.progress_text = "上次未完成，已暂停，可继续".into();
            }
        }
        let mut rooms = db::load_rooms(&db)?;
        let mut recordings = db::load_recordings(&db)?;
        recover_rooms(&db, &mut rooms, &mut recordings)?;
        let tools = ToolPaths::resolve(&settings);
        let api_port = http_api::serve(tx.clone()).unwrap_or(0);
        let mut supervisor = Self {
            db,
            snap,
            tx,
            rx,
            waker,
            settings,
            tasks,
            rooms,
            recordings,
            parse: ParseView::Idle,
            parsed: None,
            parsing_url: String::new(),
            notice: String::new(),
            notice_error: false,
            jobs: HashMap::new(),
            tools,
            api_port,
            lan_urls: Vec::new(),
            dirty: false,
            last_flush: Instant::now(),
            stop: false,
            update: UpdateInfo::default(),
            update_manual: false,
            exit_requested: false,
        };
        supervisor.refresh_urls();
        if !supervisor.tools.ytdlp.is_some() {
            supervisor.notice = "没有找到 yt-dlp。程序会使用 N:\\deep\\tools\\yt-dlp.exe，或你放在本目录 tools 里的同名文件。".into();
            supervisor.notice_error = true;
        }
        Ok(supervisor)
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Parse(url) => self.spawn_parse(url),
            Cmd::ParseDone(result) => self.on_parse(result),
            Cmd::DownloadParsed { format_id, container } => self.download_parsed(format_id, container),
            Cmd::EnqueueUrl { url, title } => {
                if let Err(err) = self.enqueue_url(&url, &title, "best", "最佳画质", "mp4", "", "", 0, "", 0) {
                    self.notify(&err, true);
                }
            }
            Cmd::Pause(id) => self.pause(&id),
            Cmd::Resume(id) => self.resume(&id),
            Cmd::Cancel(id) => self.cancel_task(&id),
            Cmd::Retry(id) => self.resume(&id),
            Cmd::DeleteTask { id, delete_file } => self.delete_task(&id, delete_file),
            Cmd::AddRoom { url, quality, format, autostart } => {
                if let Err(err) = self.add_room(&url, &quality, &format, autostart) {
                    self.notify(&err, true);
                }
            }
            Cmd::StartRoom(id) => self.start_room(&id),
            Cmd::StopRoom(id) => self.stop_room(&id),
            Cmd::DeleteRoom(id) => self.delete_room(&id),
            Cmd::CheckRoom(id) => self.check_room(id),
            Cmd::StartAllLive => self.start_all_live(),
            Cmd::StopAllLive => self.stop_all_live(),
            Cmd::ClearIdleRooms => self.clear_idle_rooms(),
            Cmd::DeleteRecording { id, delete_file } => self.delete_recording(&id, delete_file),
            Cmd::Reveal(path) => tools::reveal(Path::new(&path)),
            Cmd::OpenDir(path) => tools::open_dir(Path::new(&path)),
            Cmd::Preview { url, platform } => self.preview(&url, &platform),
            Cmd::SetSettings(settings) => self.set_settings(settings),
            Cmd::DismissNotice => {
                self.notice.clear();
                self.notice_error = false;
                self.publish();
            }
            Cmd::Ingest { kind, body, reply } => {
                let result = match kind {
                    IngestKind::Extension => self.ingest_extension(&body),
                    IngestKind::Mobile => self.ingest_mobile(&body),
                };
                let _ = reply.send(result);
            }
            Cmd::JobProgress { id, downloaded, total, speed, eta } => self.on_progress(&id, downloaded, total, speed, eta),
            Cmd::JobExit { id, code, path, error, cancelled } => self.on_job_exit(&id, code, path, error, cancelled),
            Cmd::LiveMeta { id, is_live, offline, record_url, alt_url, anchor, title, error, source } => {
                self.on_live_meta(&id, is_live, offline, record_url, alt_url, anchor, title, error, source);
            }
            Cmd::LiveBegan { id, capture_path } => self.on_live_began(&id, capture_path),
            Cmd::LiveProgress { id, size, speed } => self.on_live_progress(&id, size, speed),
            Cmd::LiveExit { id, capture_path, final_path, size, duration, error, cancelled, remuxed } => {
                self.on_live_exit(&id, capture_path, final_path, size, duration, error, cancelled, remuxed);
            }
            Cmd::Checked { id, is_live, anchor, title, error } => self.on_checked(&id, is_live, anchor, title, error),
            Cmd::CheckUpdate => self.begin_update_check(true),
            Cmd::UpdateReport(report) => self.on_update(report),
            Cmd::ApplyUpdate => self.begin_apply_update(),
            Cmd::UpdateInstalled => {
                self.exit_requested = true;
                self.update.message = "正在重启…".into();
                self.publish();
            }
            Cmd::UpdateFailed(err) => {
                self.update.phase = UpdatePhase::Failed;
                self.update.message = err;
                self.publish();
            }
            Cmd::DismissUpdate => {
                self.update.banner_dismissed = true;
                self.publish();
            }
            Cmd::Shutdown => self.stop = true,
        }
    }

    fn spawn_parse(&mut self, url: String) {
        self.parse = ParseView::Busy;
        self.parsed = None;
        self.parsing_url = url.clone();
        self.publish();
        let bin = self.tools.ytdlp.clone();
        let timeout = self.settings.timeout_sec.max(5) as u64;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = match bin {
                Some(bin) => ytdlp::parse(&bin, &url, timeout),
                None => Err("未找到 yt-dlp".into()),
            };
            let _ = tx.send(Cmd::ParseDone(result));
        });
    }

    fn on_parse(&mut self, result: Result<ParseResult, String>) {
        match result {
            Ok(parsed) => {
                self.parsing_url.clear();
                self.parse = ParseView::Ready;
                self.parsed = Some(parsed);
                self.notify("解析完成，选择画质后开始下载", false);
            }
            Err(mut err) => {
                if platform::looks_like_live(&self.parsing_url) {
                    err.push_str("。这像是直播链接，可以到「直播」页录制");
                }
                self.parse = ParseView::Failed(err.clone());
                self.parsed = None;
                self.notify(&err, true);
            }
        }
    }

    fn download_parsed(&mut self, format_id: String, container: String) {
        let Some(parsed) = self.parsed.clone() else {
            self.notify("先解析一个链接", true);
            return;
        };
        let chosen = parsed.formats.iter().find(|f| f.id == format_id).cloned();
        let quality = chosen.as_ref().map(|f| f.label.clone()).unwrap_or_else(|| "最佳画质".into());
        let ext = chosen.as_ref().map(|f| f.ext.clone()).unwrap_or_else(|| container.clone());
        let filesize = chosen.as_ref().map(|f| f.filesize).unwrap_or(0);
        if let Err(err) = self.enqueue_url(
            &parsed.url,
            &parsed.title,
            &format_id,
            &quality,
            &container,
            &parsed.author,
            &parsed.thumbnail,
            parsed.duration,
            &ext,
            filesize,
        ) {
            self.notify(&err, true);
            return;
        }
        self.parse = ParseView::Idle;
        self.parsed = None;
    }

    fn enqueue_url(
        &mut self,
        url: &str,
        title: &str,
        format_id: &str,
        quality: &str,
        container: &str,
        author: &str,
        thumbnail: &str,
        duration: i64,
        ext: &str,
        filesize: i64,
    ) -> Result<(), String> {
        let url = platform::extract_url(url).ok_or("未找到有效的 http(s) 链接")?;
        if self.tasks.iter().any(|task| {
            task.url == url && matches!(task.status, TaskStatus::Waiting | TaskStatus::Parsing | TaskStatus::Downloading | TaskStatus::Paused)
        }) {
            return Err("任务已存在".into());
        }
        let info = platform::detect_platform(&url);
        let mut task = Task::blank(url);
        task.title = if title.is_empty() { task.url.clone() } else { title.to_string() };
        task.platform = info.id;
        task.platform_label = info.label;
        task.author = author.to_string();
        task.thumbnail = thumbnail.to_string();
        task.duration = duration;
        task.quality = quality.to_string();
        task.container = if container.is_empty() { "mp4".into() } else { container.into() };
        task.format_id = if format_id.is_empty() { "best".into() } else { format_id.into() };
        task.ext = if ext.is_empty() { task.container.clone() } else { ext.into() };
        task.filesize = filesize;
        task.save_dir = self.settings.download_dir.clone();
        task.progress_text = "等待下载".into();
        db::save_task(&self.db, &task)?;
        self.tasks.insert(0, task);
        self.notify("已加入下载队列", false);
        self.pump_downloads();
        self.publish();
        Ok(())
    }

    fn pause(&mut self, id: &str) {
        let Some(idx) = self.task_index(id) else { return };
        match self.tasks[idx].status {
            TaskStatus::Waiting => {
                self.tasks[idx].status = TaskStatus::Paused;
                self.tasks[idx].progress_text = "已暂停".into();
            }
            TaskStatus::Downloading | TaskStatus::Parsing => {
                self.stop_job(id);
                self.tasks[idx].status = TaskStatus::Paused;
                self.tasks[idx].speed = 0.0;
                self.tasks[idx].progress_text = "已暂停".into();
            }
            _ => return,
        }
        self.tasks[idx].updated_at = now_ms();
        self.persist_task(id);
        self.publish();
    }

    fn resume(&mut self, id: &str) {
        if self.jobs.contains_key(id) {
            self.notify("上一次下载还在停止，请稍后再继续", true);
            return;
        }
        let Some(idx) = self.task_index(id) else { return };
        if matches!(self.tasks[idx].status, TaskStatus::Completed) {
            return;
        }
        self.tasks[idx].status = TaskStatus::Waiting;
        self.tasks[idx].error.clear();
        self.tasks[idx].progress_text = "等待下载".into();
        self.tasks[idx].updated_at = now_ms();
        self.persist_task(id);
        self.pump_downloads();
        self.publish();
    }

    fn cancel_task(&mut self, id: &str) {
        self.stop_job(id);
        let Some(idx) = self.task_index(id) else { return };
        if matches!(self.tasks[idx].status, TaskStatus::Completed) {
            return;
        }
        self.tasks[idx].status = TaskStatus::Cancelled;
        self.tasks[idx].speed = 0.0;
        self.tasks[idx].progress_text = "已取消".into();
        self.tasks[idx].updated_at = now_ms();
        self.persist_task(id);
        self.publish();
    }

    fn delete_task(&mut self, id: &str, delete_file: bool) {
        self.stop_job(id);
        if delete_file {
            if let Some(task) = self.tasks.iter().find(|task| task.id == id) {
                if !task.output_path.is_empty() {
                    let _ = std::fs::remove_file(&task.output_path);
                }
            }
        }
        if let Err(err) = db::delete_task(&self.db, id) {
            self.notify(&err, true);
            return;
        }
        self.tasks.retain(|task| task.id != id);
        self.publish();
    }

    fn add_room(&mut self, url: &str, quality: &str, format: &str, autostart: bool) -> Result<String, String> {
        let url = platform::normalize_live_url(url);
        if url.is_empty() {
            return Err("空链接".into());
        }
        if let Some(idx) = self.rooms.iter().position(|room| room.url == url) {
            if autostart && !self.rooms[idx].status.is_running() {
                let id = self.rooms[idx].id.clone();
                self.rooms[idx].quality = quality.to_string();
                self.rooms[idx].format = format.to_string();
                self.queue_room(idx);
                self.pump_live();
                self.publish();
                return Ok(id);
            }
            return Err("这个直播间已在列表中".into());
        }
        let mut room = blank_room(url, quality.to_string(), format.to_string());
        if autostart {
            room.status = RoomStatus::Queued;
            room.message = "排队中".into();
        }
        db::save_room(&self.db, &room)?;
        let id = room.id.clone();
        self.rooms.insert(0, room);
        self.notify("已加入直播列表", false);
        self.pump_live();
        self.publish();
        Ok(id)
    }

    fn start_room(&mut self, id: &str) {
        let Some(idx) = self.room_index(id) else { return };
        if self.rooms[idx].status.is_running() || self.jobs.contains_key(id) {
            return;
        }
        self.queue_room(idx);
        self.pump_live();
        self.publish();
    }

    fn stop_room(&mut self, id: &str) {
        let Some(idx) = self.room_index(id) else { return };
        if self.rooms[idx].status == RoomStatus::Queued && !self.jobs.contains_key(id) {
            self.rooms[idx].status = RoomStatus::Idle;
            self.rooms[idx].message = "已取消排队".into();
            self.rooms[idx].updated_at = now_ms();
            self.persist_room(id);
            self.publish();
            return;
        }
        self.stop_job(id);
        if self.rooms[idx].status.is_running() {
            self.rooms[idx].status = RoomStatus::Stopping;
            self.rooms[idx].message = "正在停止…".into();
            self.rooms[idx].updated_at = now_ms();
            self.persist_room(id);
            self.publish();
        }
    }

    fn delete_room(&mut self, id: &str) {
        self.stop_job(id);
        if let Err(err) = db::delete_room(&self.db, id) {
            self.notify(&err, true);
            return;
        }
        self.rooms.retain(|room| room.id != id);
        self.publish();
    }

    fn check_room(&mut self, id: String) {
        let Some(idx) = self.room_index(&id) else { return };
        if self.rooms[idx].status == RoomStatus::Recording {
            return;
        }
        let url = self.rooms[idx].url.clone();
        let quality = self.rooms[idx].quality.clone();
        let ytdlp = self.tools.ytdlp.clone();
        self.rooms[idx].message = "正在检查是否开播…".into();
        self.publish();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let info = live::resolve(&url, &quality, ytdlp.as_deref());
            let _ = tx.send(Cmd::Checked {
                id,
                is_live: info.is_live && !info.record_url.is_empty(),
                anchor: info.anchor_name,
                title: info.title,
                error: info.error,
            });
        });
    }

    fn start_all_live(&mut self) {
        let ids: Vec<String> = self
            .rooms
            .iter()
            .filter(|room| !room.status.is_running())
            .map(|room| room.id.clone())
            .collect();
        for id in ids {
            if let Some(idx) = self.room_index(&id) {
                self.queue_room(idx);
            }
        }
        self.pump_live();
        self.publish();
    }

    fn stop_all_live(&mut self) {
        let ids: Vec<String> = self.rooms.iter().filter(|room| room.status.is_running()).map(|room| room.id.clone()).collect();
        for id in ids {
            self.stop_room(&id);
        }
    }

    fn clear_idle_rooms(&mut self) {
        let ids: Vec<String> = self
            .rooms
            .iter()
            .filter(|room| !room.status.is_running() && !self.jobs.contains_key(&room.id))
            .map(|room| room.id.clone())
            .collect();
        for id in &ids {
            let _ = db::delete_room(&self.db, id);
        }
        self.rooms.retain(|room| !ids.iter().any(|id| id == &room.id));
        self.publish();
    }

    fn delete_recording(&mut self, id: &str, delete_file: bool) {
        if delete_file {
            if let Some(item) = self.recordings.iter().find(|item| item.id == id) {
                if !item.output_path.is_empty() {
                    let _ = std::fs::remove_file(&item.output_path);
                }
            }
        }
        if let Err(err) = db::delete_recording(&self.db, id) {
            self.notify(&err, true);
            return;
        }
        self.recordings.retain(|item| item.id != id);
        self.publish();
    }

    fn preview(&mut self, url: &str, platform: &str) {
        let Some(ffplay) = self.tools.ffplay.clone() else {
            self.notify("没有找到 ffplay，不能预览。它和 ffmpeg 放在同一目录。", true);
            return;
        };
        if let Err(err) = live::preview(&ffplay, url, platform) {
            self.notify(&err, true);
        }
    }

    fn set_settings(&mut self, mut settings: Settings) {
        if settings.download_dir.trim().is_empty() {
            settings.download_dir = self.settings.download_dir.clone();
        }
        if settings.live_dir.trim().is_empty() {
            settings.live_dir = self.settings.live_dir.clone();
        }
        settings.max_concurrency = settings.max_concurrency.clamp(1, 10);
        settings.live_max_concurrent = settings.live_max_concurrent.clamp(1, 12);
        settings.download_threads = settings.download_threads.clamp(1, 16);
        let _ = std::fs::create_dir_all(&settings.download_dir);
        let _ = std::fs::create_dir_all(&settings.live_dir);
        if let Err(err) = db::save_settings(&self.db, &settings) {
            self.notify(&err, true);
            return;
        }
        self.settings = settings;
        self.tools = ToolPaths::resolve(&self.settings);
        self.notify("设置已保存", false);
        self.pump_downloads();
        self.pump_live();
        self.publish();
    }

    fn ingest_extension(&mut self, body: &str) -> Result<IngestResult, String> {
        let incoming = http_api::parse_extension_body(body)?;
        self.ingest_url(&incoming.url, &incoming.action, &incoming.title)
    }

    fn ingest_mobile(&mut self, body: &str) -> Result<IngestResult, String> {
        let incoming = http_api::parse_mobile_body(body)?;
        self.ingest_url(&incoming.url, &incoming.action, &incoming.title)
    }

    fn ingest_url(&mut self, raw: &str, action: &str, title: &str) -> Result<IngestResult, String> {
        let url = platform::extract_url(raw).ok_or("未找到有效的 http(s) 链接")?;
        if platform::classify(action, &url) == "live" {
            self.add_room(&url, &self.settings.live_quality.clone(), &self.settings.live_format.clone(), true)?;
            Ok(IngestResult { kind: "live".into(), message: format!("已加入直播录制：{url}") })
        } else {
            self.enqueue_url(&url, title, "best", "最佳画质", "mp4", "", "", 0, "", 0)?;
            let shown = if title.is_empty() { url } else { title.to_string() };
            Ok(IngestResult { kind: "download".into(), message: format!("已加入下载队列：{shown}") })
        }
    }

    fn on_progress(&mut self, id: &str, downloaded: u64, total: u64, speed: f64, eta: i64) {
        let Some(idx) = self.task_index(id) else { return };
        if self.tasks[idx].status != TaskStatus::Downloading {
            return;
        }
        let task = &mut self.tasks[idx];
        task.downloaded_bytes = downloaded as i64;
        if total > 0 {
            task.total_bytes = total as i64;
            task.percent = (downloaded as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
            if task.filesize == 0 {
                task.filesize = total as i64;
            }
        }
        task.speed = speed;
        task.eta = eta;
        task.progress_text = format!(
            "{} / {}",
            human_bytes(downloaded),
            if total == 0 { "未知大小".to_string() } else { human_bytes(total) }
        );
        task.updated_at = now_ms();
        self.sync_task(id);
        self.dirty = true;
    }

    fn on_job_exit(&mut self, id: &str, code: i32, path: String, error: String, cancelled: bool) {
        self.jobs.remove(id);
        let Some(idx) = self.task_index(id) else {
            self.pump_downloads();
            return;
        };
        if cancelled || matches!(self.tasks[idx].status, TaskStatus::Paused | TaskStatus::Cancelled) {
            if !path.is_empty() {
                self.tasks[idx].output_path = path;
            }
            self.tasks[idx].speed = 0.0;
            self.persist_task(id);
            self.publish();
            self.pump_downloads();
            return;
        }
        let now = now_ms();
        if code == 0 && !path.is_empty() {
            self.tasks[idx].status = TaskStatus::Completed;
            self.tasks[idx].percent = 100.0;
            self.tasks[idx].error.clear();
            self.tasks[idx].progress_text = "完成".into();
            self.tasks[idx].completed_at = now;
            self.tasks[idx].output_path = path.clone();
            let size = tools::file_size(Path::new(&path)) as i64;
            if size > 0 {
                self.tasks[idx].filesize = size;
                self.tasks[idx].downloaded_bytes = size;
                self.tasks[idx].total_bytes = size;
            }
            if self.tasks[idx].title.is_empty() || self.tasks[idx].title == self.tasks[idx].url {
                if let Some(stem) = Path::new(&path).file_stem() {
                    self.tasks[idx].title = stem.to_string_lossy().into_owned();
                }
            }
        } else {
            self.tasks[idx].status = TaskStatus::Failed;
            self.tasks[idx].error = if error.is_empty() { "下载失败".into() } else { error };
            self.tasks[idx].progress_text = "失败".into();
            if !path.is_empty() {
                self.tasks[idx].output_path = path;
            }
        }
        self.tasks[idx].speed = 0.0;
        self.tasks[idx].updated_at = now;
        self.persist_task(id);
        self.pump_downloads();
        self.publish();
    }

    fn on_live_meta(
        &mut self,
        id: &str,
        is_live: bool,
        offline: bool,
        record_url: String,
        alt_url: String,
        anchor: String,
        title: String,
        error: String,
        source: String,
    ) {
        let Some(idx) = self.room_index(id) else { return };
        if source == "resolving" {
            self.rooms[idx].status = RoomStatus::Resolving;
            self.rooms[idx].message = "正在解析直播间…".into();
        } else {
            if !anchor.is_empty() {
                self.rooms[idx].anchor_name = anchor;
            }
            if !title.is_empty() {
                self.rooms[idx].title = title;
            }
            self.rooms[idx].record_url = record_url;
            self.rooms[idx].alt_url = alt_url;
            self.rooms[idx].live_on = Some(is_live && !offline);
            if !error.is_empty() && self.rooms[idx].status != RoomStatus::Recording {
                self.rooms[idx].error = error.clone();
                self.rooms[idx].message = error;
            }
        }
        self.rooms[idx].updated_at = now_ms();
        self.persist_room(id);
        self.publish();
    }

    fn on_live_began(&mut self, id: &str, capture_path: String) {
        let Some(idx) = self.room_index(id) else { return };
        let now = now_ms();
        let recording = LiveRecording {
            id: new_id(),
            room_id: self.rooms[idx].id.clone(),
            url: self.rooms[idx].url.clone(),
            platform: self.rooms[idx].platform.clone(),
            platform_label: self.rooms[idx].platform_label.clone(),
            anchor_name: self.rooms[idx].anchor_name.clone(),
            title: self.rooms[idx].title.clone(),
            quality: self.rooms[idx].quality.clone(),
            format: "ts".into(),
            status: RecordingStatus::Recording,
            error: String::new(),
            output_path: capture_path.clone(),
            filesize: 0,
            duration_sec: 0,
            started_at: now,
            ended_at: 0,
            created_at: now,
        };
        if let Err(err) = db::save_recording(&self.db, &recording) {
            log::warn!("recording: {err}");
        }
        self.rooms[idx].current_recording_id = recording.id.clone();
        self.rooms[idx].output_path = capture_path;
        self.rooms[idx].status = RoomStatus::Recording;
        self.rooms[idx].message = "录制中".into();
        self.rooms[idx].error.clear();
        self.rooms[idx].started_at = now;
        self.rooms[idx].updated_at = now;
        self.recordings.insert(0, recording);
        self.persist_room(id);
        self.publish();
    }

    fn on_live_progress(&mut self, id: &str, size: u64, speed: f64) {
        let Some(idx) = self.room_index(id) else { return };
        if self.rooms[idx].status != RoomStatus::Recording {
            return;
        }
        self.rooms[idx].filesize = size as i64;
        self.rooms[idx].speed = speed;
        self.rooms[idx].updated_at = now_ms();
        let rec_id = self.rooms[idx].current_recording_id.clone();
        if let Some(item) = self.recordings.iter_mut().find(|item| item.id == rec_id) {
            item.filesize = size as i64;
        }
        self.sync_room(id);
        self.dirty = true;
    }

    fn on_live_exit(
        &mut self,
        id: &str,
        capture_path: String,
        final_path: String,
        size: u64,
        duration: i64,
        error: String,
        cancelled: bool,
        remuxed: bool,
    ) {
        self.jobs.remove(id);
        let Some(idx) = self.room_index(id) else { return };
        let now = now_ms();
        let rec_id = self.rooms[idx].current_recording_id.clone();
        let kept = if !final_path.is_empty() { final_path } else { capture_path };
        let status = if size == 0 && cancelled {
            RecordingStatus::Cancelled
        } else if size == 0 {
            RecordingStatus::Failed
        } else if cancelled || !remuxed {
            RecordingStatus::Partial
        } else {
            RecordingStatus::Completed
        };
        if let Some(item) = self.recordings.iter_mut().find(|item| item.id == rec_id) {
            item.status = status;
            item.output_path = kept.clone();
            item.filesize = size as i64;
            item.duration_sec = duration;
            item.ended_at = now;
            item.error = error.clone();
            if let Some(ext) = Path::new(&item.output_path).extension().and_then(|s| s.to_str()) {
                item.format = ext.to_string();
            }
            let saved = item.clone();
            let _ = db::save_recording(&self.db, &saved);
        }
        self.rooms[idx].status = if status == RecordingStatus::Failed { RoomStatus::Error } else { RoomStatus::Done };
        self.rooms[idx].message = match status {
            RecordingStatus::Completed => "录制完成".into(),
            RecordingStatus::Partial => "录制已停止，文件可能不完整".into(),
            RecordingStatus::Cancelled => "已取消".into(),
            RecordingStatus::Failed => if error.is_empty() { "录制失败".into() } else { error.clone() },
            RecordingStatus::Recording => "已结束".into(),
        };
        self.rooms[idx].error = if status == RecordingStatus::Failed { error } else { String::new() };
        self.rooms[idx].output_path = kept;
        self.rooms[idx].filesize = size as i64;
        self.rooms[idx].speed = 0.0;
        self.rooms[idx].current_recording_id.clear();
        self.rooms[idx].updated_at = now;
        self.persist_room(id);
        self.pump_live();
        self.publish();
    }

    fn on_checked(&mut self, id: &str, is_live: bool, anchor: String, title: String, error: String) {
        let Some(idx) = self.room_index(id) else { return };
        if self.rooms[idx].status == RoomStatus::Recording {
            return;
        }
        if !anchor.is_empty() {
            self.rooms[idx].anchor_name = anchor;
        }
        if !title.is_empty() {
            self.rooms[idx].title = title;
        }
        self.rooms[idx].live_on = Some(is_live);
        self.rooms[idx].message = if is_live {
            "正在直播".into()
        } else if error.is_empty() {
            "未开播".into()
        } else {
            error
        };
        self.rooms[idx].updated_at = now_ms();
        self.persist_room(id);
        self.publish();
    }

    fn pump_downloads(&mut self) {
        let max = self.settings.max_concurrency.max(1) as usize;
        loop {
            let running = self.jobs.values().filter(|job| matches!(job.kind, JobKind::Download)).count();
            if running >= max {
                break;
            }
            let Some(id) = self
                .tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Waiting)
                .min_by_key(|task| task.created_at)
                .map(|task| task.id.clone())
            else {
                break;
            };
            self.spawn_download(id);
        }
    }

    fn pump_live(&mut self) {
        let max = self.settings.live_max_concurrent.max(1) as usize;
        loop {
            let running = self.jobs.values().filter(|job| matches!(job.kind, JobKind::Live)).count();
            if running >= max {
                break;
            }
            let Some(id) = self
                .rooms
                .iter()
                .filter(|room| room.status == RoomStatus::Queued)
                .min_by_key(|room| room.updated_at)
                .map(|room| room.id.clone())
            else {
                break;
            };
            self.spawn_live(id);
        }
    }

    fn spawn_download(&mut self, id: String) {
        if self.jobs.contains_key(&id) {
            return;
        }
        let Some(idx) = self.task_index(&id) else { return };
        self.tasks[idx].status = TaskStatus::Downloading;
        self.tasks[idx].error.clear();
        self.tasks[idx].progress_text = "正在下载…".into();
        if self.tasks[idx].started_at == 0 {
            self.tasks[idx].started_at = now_ms();
        }
        self.tasks[idx].updated_at = now_ms();
        let task = self.tasks[idx].clone();
        self.persist_task(&id);
        let flag = Arc::new(CancelFlag::new());
        self.jobs.insert(id, Job { flag: Arc::clone(&flag), kind: JobKind::Download });
        let tx = self.tx.clone();
        let settings = self.settings.clone();
        let ytdlp = self.tools.ytdlp.clone();
        let ffmpeg = self.tools.ffmpeg.clone();
        thread::spawn(move || ytdlp::download(tx, task, settings, ytdlp, ffmpeg, flag));
    }

    fn spawn_live(&mut self, id: String) {
        if self.jobs.contains_key(&id) {
            return;
        }
        let Some(idx) = self.room_index(&id) else { return };
        self.rooms[idx].status = RoomStatus::Resolving;
        self.rooms[idx].message = "正在解析直播间…".into();
        self.rooms[idx].error.clear();
        self.rooms[idx].updated_at = now_ms();
        let url = self.rooms[idx].url.clone();
        let quality = self.rooms[idx].quality.clone();
        let format = self.rooms[idx].format.clone();
        let platform_hint = self.rooms[idx].platform.clone();
        self.persist_room(&id);
        let flag = Arc::new(CancelFlag::new());
        self.jobs.insert(id.clone(), Job { flag: Arc::clone(&flag), kind: JobKind::Live });
        let tx = self.tx.clone();
        let live_dir = std::path::PathBuf::from(&self.settings.live_dir);
        let ytdlp = self.tools.ytdlp.clone();
        let ffmpeg = self.tools.ffmpeg.clone();
        let ffprobe = self.tools.ffprobe.clone();
        thread::spawn(move || {
            live::run_record(tx, id, url, quality, format, platform_hint, live_dir, ytdlp, ffmpeg, ffprobe, flag);
        });
    }

    fn queue_room(&mut self, idx: usize) {
        self.rooms[idx].status = RoomStatus::Queued;
        self.rooms[idx].error.clear();
        self.rooms[idx].message = "排队中".into();
        self.rooms[idx].updated_at = now_ms();
        let room = self.rooms[idx].clone();
        let _ = db::save_room(&self.db, &room);
    }

    fn stop_job(&mut self, id: &str) {
        if let Some(job) = self.jobs.get(id) {
            job.flag.cancel();
            tools::kill_pid(job.flag.pid());
        }
    }

    fn flush(&mut self) {
        if !self.dirty || self.last_flush.elapsed() < Duration::from_secs(1) {
            return;
        }
        for task in &self.tasks {
            if task.status == TaskStatus::Downloading {
                let _ = db::save_task(&self.db, task);
            }
        }
        for room in &self.rooms {
            if room.status == RoomStatus::Recording {
                let _ = db::save_room(&self.db, room);
            }
        }
        self.dirty = false;
        self.last_flush = Instant::now();
    }

    fn persist_task(&mut self, id: &str) {
        if let Some(task) = self.tasks.iter().find(|task| task.id == id).cloned() {
            if let Err(err) = db::save_task(&self.db, &task) {
                log::warn!("save task: {err}");
            }
        }
    }

    fn persist_room(&mut self, id: &str) {
        if let Some(room) = self.rooms.iter().find(|room| room.id == id).cloned() {
            if let Err(err) = db::save_room(&self.db, &room) {
                log::warn!("save room: {err}");
            }
        }
    }

    fn sync_task(&self, id: &str) {
        let Some(task) = self.tasks.iter().find(|task| task.id == id).cloned() else { return };
        let mut snap = self.snap.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(slot) = snap.tasks.iter_mut().find(|task| task.id == id) {
            *slot = task;
        }
        snap.busy = true;
        drop(snap);
        self.waker.wake();
    }

    fn sync_room(&self, id: &str) {
        let Some(room) = self.rooms.iter().find(|room| room.id == id).cloned() else { return };
        let rec_id = room.current_recording_id.clone();
        let recording = self.recordings.iter().find(|item| item.id == rec_id).cloned();
        let mut snap = self.snap.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(slot) = snap.rooms.iter_mut().find(|room| room.id == id) {
            *slot = room;
        }
        if let Some(recording) = recording {
            if let Some(slot) = snap.recordings.iter_mut().find(|item| item.id == recording.id) {
                *slot = recording;
            }
        }
        snap.busy = true;
        drop(snap);
        self.waker.wake();
    }

    fn begin_update_check(&mut self, manual: bool) {
        if matches!(self.update.phase, UpdatePhase::Checking | UpdatePhase::Downloading) {
            return;
        }
        self.update_manual = manual;
        self.update.phase = UpdatePhase::Checking;
        if manual {
            self.update.message = "正在检查更新…".into();
            self.publish();
        }
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Cmd::UpdateReport(update::check()));
        });
    }

    fn on_update(&mut self, report: UpdateReport) {
        if self.exit_requested || self.update.phase == UpdatePhase::Downloading {
            return;
        }
        let manual = self.update_manual;
        self.update_manual = false;
        match report {
            UpdateReport::None => {
                self.update.phase = UpdatePhase::Current;
                self.update.version.clear();
                self.update.asset_url.clear();
                self.update.message = if manual { "还没有发布版。".into() } else { String::new() };
            }
            UpdateReport::Current => {
                self.update.phase = UpdatePhase::Current;
                self.update.version.clear();
                self.update.asset_url.clear();
                self.update.message = if manual { "已经是最新版本。".into() } else { String::new() };
            }
            UpdateReport::Available(info) => {
                let version = info.version.clone();
                self.update.phase = UpdatePhase::Available;
                self.update.banner_dismissed = false;
                self.update.version = info.version;
                self.update.notes = info.notes;
                self.update.asset_url = info.asset_url;
                self.update.asset_size = info.asset_size;
                self.update.message = format!("发现新版本 {version}");
            }
            UpdateReport::Failed(err) => {
                if manual {
                    self.update.phase = UpdatePhase::Failed;
                    self.update.message = err;
                } else {
                    self.update.phase = UpdatePhase::Idle;
                    self.update.message.clear();
                }
            }
        }
        self.publish();
    }

    fn begin_apply_update(&mut self) {
        if self.update.phase == UpdatePhase::Downloading || self.update.asset_url.is_empty() {
            return;
        }
        if self.work_busy() {
            self.notify("正在下载或录制，停掉之后再更新。", true);
            return;
        }
        let url = self.update.asset_url.clone();
        let size = self.update.asset_size;
        self.update.phase = UpdatePhase::Downloading;
        self.update.message = "正在下载新版本…".into();
        self.publish();
        let tx = self.tx.clone();
        thread::spawn(move || match update::download_and_swap(&url, size) {
            Ok(()) => {
                let _ = tx.send(Cmd::UpdateInstalled);
            }
            Err(err) => {
                let _ = tx.send(Cmd::UpdateFailed(err));
            }
        });
    }

    fn work_busy(&self) -> bool {
        self.tasks.iter().any(|task| matches!(task.status, TaskStatus::Downloading | TaskStatus::Parsing))
            || self.rooms.iter().any(|room| room.status.is_running())
    }

    fn publish(&mut self) {
        if let Ok(stats) = db::stats(&self.db) {
            let mut snap = self.snap.lock().unwrap_or_else(|err| err.into_inner());
            snap.stats = stats;
        }
        let busy = self.tasks.iter().any(|task| matches!(task.status, TaskStatus::Downloading | TaskStatus::Parsing))
            || self.rooms.iter().any(|room| room.status.is_running());
        let mut snap = self.snap.lock().unwrap_or_else(|err| err.into_inner());
        snap.ready = true;
        snap.boot_error.clear();
        snap.settings = self.settings.clone();
        snap.tasks = self.tasks.clone();
        snap.rooms = self.rooms.clone();
        snap.recordings = self.recordings.clone();
        snap.parse = self.parse.clone();
        snap.parsed = self.parsed.clone();
        snap.notice = self.notice.clone();
        snap.notice_error = self.notice_error;
        snap.tools = self.tools.status();
        snap.lan_urls = self.lan_urls.clone();
        snap.api_port = self.api_port;
        snap.busy = busy;
        snap.update = self.update.clone();
        snap.request_quit = self.exit_requested;
        drop(snap);
        self.waker.wake();
    }

    fn notify(&mut self, text: &str, error: bool) {
        self.notice = text.to_string();
        self.notice_error = error;
        self.publish();
    }

    fn refresh_urls(&mut self) {
        self.lan_urls.clear();
        if self.api_port == 0 {
            self.lan_urls.push("4000、41777、18765 都被占用，手机和浏览器扩展连不上。".into());
            return;
        }
        self.lan_urls.push(format!("http://127.0.0.1:{}/api/mobile/link", self.api_port));
        if let Some(ip) = tools::lan_ip() {
            self.lan_urls.push(format!("http://{ip}:{}/api/mobile/link", self.api_port));
        }
    }

    fn task_index(&self, id: &str) -> Option<usize> {
        self.tasks.iter().position(|task| task.id == id)
    }

    fn room_index(&self, id: &str) -> Option<usize> {
        self.rooms.iter().position(|room| room.id == id)
    }
}

fn blank_room(url: String, quality: String, format: String) -> LiveRoom {
    let info = platform::detect_live(&url);
    let now = now_ms();
    LiveRoom {
        id: new_id(),
        url,
        quality,
        format,
        platform: info.id,
        platform_label: info.label,
        anchor_name: String::new(),
        title: String::new(),
        thumbnail: String::new(),
        status: RoomStatus::Idle,
        message: String::new(),
        error: String::new(),
        output_path: String::new(),
        record_url: String::new(),
        alt_url: String::new(),
        current_recording_id: String::new(),
        filesize: 0,
        speed: 0.0,
        started_at: 0,
        created_at: now,
        updated_at: now,
        live_on: None,
    }
}

fn recover_rooms(db: &Connection, rooms: &mut [LiveRoom], recordings: &mut Vec<LiveRecording>) -> Result<(), String> {
    let now = now_ms();
    for room in rooms.iter_mut() {
        if !matches!(room.status, RoomStatus::Resolving | RoomStatus::Recording | RoomStatus::Stopping | RoomStatus::Queued) {
            continue;
        }
        let size = if room.output_path.is_empty() { 0 } else { tools::file_size(Path::new(&room.output_path)) };
        if size > 0 {
            let rec_id = if room.current_recording_id.is_empty() { new_id() } else { room.current_recording_id.clone() };
            let item = LiveRecording {
                id: rec_id,
                room_id: room.id.clone(),
                url: room.url.clone(),
                platform: room.platform.clone(),
                platform_label: room.platform_label.clone(),
                anchor_name: room.anchor_name.clone(),
                title: room.title.clone(),
                quality: room.quality.clone(),
                format: Path::new(&room.output_path)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ts")
                    .to_string(),
                status: RecordingStatus::Partial,
                error: "上次录制因退出中断，文件已保留".into(),
                output_path: room.output_path.clone(),
                filesize: size as i64,
                duration_sec: 0,
                started_at: room.started_at,
                ended_at: now,
                created_at: room.created_at,
            };
            db::save_recording(db, &item)?;
            if let Some(slot) = recordings.iter_mut().find(|rec| rec.id == item.id) {
                *slot = item;
            } else {
                recordings.insert(0, item);
            }
            room.status = RoomStatus::Done;
            room.message = "上次录制中断，文件已保留".into();
            room.filesize = size as i64;
        } else {
            room.status = RoomStatus::Idle;
            room.message = "上次没有录完".into();
        }
        room.speed = 0.0;
        room.current_recording_id.clear();
        room.updated_at = now;
        db::save_room(db, room)?;
    }
    Ok(())
}
