use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::mpsc::Sender;

use crate::model::{IngestResult, ParseResult, Settings};
use crate::update::UpdateReport;

pub struct CancelFlag {
    pub pid: AtomicU32,
    pub cancel: AtomicBool,
}

impl CancelFlag {
    pub fn new() -> Self {
        Self {
            pid: AtomicU32::new(0),
            cancel: AtomicBool::new(false),
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_pid(&self, pid: u32) {
        self.pid.store(pid, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn pid(&self) -> u32 {
        self.pid.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub enum IngestKind {
    Extension,
    Mobile,
}

pub enum Cmd {
    Parse(String),
    ParseDone(Result<ParseResult, String>),
    DownloadParsed {
        format_id: String,
        container: String,
    },
    EnqueueUrl {
        url: String,
        title: String,
    },
    Pause(String),
    Resume(String),
    Cancel(String),
    Retry(String),
    DeleteTask {
        id: String,
        delete_file: bool,
    },
    AddRoom {
        url: String,
        quality: String,
        format: String,
        autostart: bool,
    },
    StartRoom(String),
    StopRoom(String),
    DeleteRoom(String),
    CheckRoom(String),
    StartAllLive,
    StopAllLive,
    ClearIdleRooms,
    DeleteRecording {
        id: String,
        delete_file: bool,
    },
    Reveal(String),
    OpenDir(String),
    Preview {
        url: String,
        platform: String,
    },
    SetSettings(Settings),
    DismissNotice,
    Ingest {
        kind: IngestKind,
        body: String,
        reply: Sender<Result<IngestResult, String>>,
    },
    JobProgress {
        id: String,
        downloaded: u64,
        total: u64,
        speed: f64,
        eta: i64,
    },
    JobExit {
        id: String,
        code: i32,
        path: String,
        error: String,
        cancelled: bool,
    },
    LiveMeta {
        id: String,
        is_live: bool,
        offline: bool,
        record_url: String,
        alt_url: String,
        anchor: String,
        title: String,
        error: String,
        source: String,
    },
    LiveBegan {
        id: String,
        capture_path: String,
    },
    LiveProgress {
        id: String,
        size: u64,
        speed: f64,
    },
    LiveExit {
        id: String,
        capture_path: String,
        final_path: String,
        size: u64,
        duration: i64,
        error: String,
        cancelled: bool,
        remuxed: bool,
    },
    Checked {
        id: String,
        is_live: bool,
        anchor: String,
        title: String,
        error: String,
    },
    CheckUpdate,
    UpdateReport(UpdateReport),
    ApplyUpdate,
    UpdateInstalled,
    UpdateFailed(String),
    DismissUpdate,
    Shutdown,
}
