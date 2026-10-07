use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::{
    LiveRecording, LiveRoom, RecordingStatus, RoomStatus, Settings, Stats, Task, TaskStatus, local_day, now_ms,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',
  platform TEXT NOT NULL DEFAULT 'generic',
  platform_label TEXT NOT NULL DEFAULT '',
  author TEXT NOT NULL DEFAULT '',
  thumbnail TEXT NOT NULL DEFAULT '',
  duration INTEGER NOT NULL DEFAULT 0,
  quality TEXT NOT NULL DEFAULT '',
  container TEXT NOT NULL DEFAULT 'mp4',
  format_id TEXT NOT NULL DEFAULT '',
  ext TEXT NOT NULL DEFAULT '',
  filesize INTEGER NOT NULL DEFAULT 0,
  downloaded_bytes INTEGER NOT NULL DEFAULT 0,
  total_bytes INTEGER NOT NULL DEFAULT 0,
  speed REAL NOT NULL DEFAULT 0,
  eta INTEGER NOT NULL DEFAULT 0,
  percent REAL NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'waiting',
  error TEXT NOT NULL DEFAULT '',
  output_path TEXT NOT NULL DEFAULT '',
  save_dir TEXT NOT NULL DEFAULT '',
  progress_text TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL DEFAULT 0,
  started_at INTEGER NOT NULL DEFAULT 0,
  completed_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_created ON tasks(created_at);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS live_rooms (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  quality TEXT NOT NULL DEFAULT 'OD',
  format TEXT NOT NULL DEFAULT 'mkv',
  platform TEXT NOT NULL DEFAULT 'generic',
  platform_label TEXT NOT NULL DEFAULT '',
  anchor_name TEXT NOT NULL DEFAULT '',
  title TEXT NOT NULL DEFAULT '',
  thumbnail TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'idle',
  message TEXT NOT NULL DEFAULT '',
  error TEXT NOT NULL DEFAULT '',
  output_path TEXT NOT NULL DEFAULT '',
  record_url TEXT NOT NULL DEFAULT '',
  alt_url TEXT NOT NULL DEFAULT '',
  current_recording_id TEXT NOT NULL DEFAULT '',
  filesize INTEGER NOT NULL DEFAULT 0,
  speed REAL NOT NULL DEFAULT 0,
  started_at INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL DEFAULT 0,
  live_on INTEGER NOT NULL DEFAULT -1
);

CREATE TABLE IF NOT EXISTS live_recordings (
  id TEXT PRIMARY KEY,
  room_id TEXT NOT NULL DEFAULT '',
  url TEXT NOT NULL DEFAULT '',
  platform TEXT NOT NULL DEFAULT 'generic',
  platform_label TEXT NOT NULL DEFAULT '',
  anchor_name TEXT NOT NULL DEFAULT '',
  title TEXT NOT NULL DEFAULT '',
  quality TEXT NOT NULL DEFAULT '',
  format TEXT NOT NULL DEFAULT 'mkv',
  status TEXT NOT NULL DEFAULT 'recording',
  error TEXT NOT NULL DEFAULT '',
  output_path TEXT NOT NULL DEFAULT '',
  filesize INTEGER NOT NULL DEFAULT 0,
  duration_sec INTEGER NOT NULL DEFAULT 0,
  started_at INTEGER NOT NULL DEFAULT 0,
  ended_at INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL DEFAULT 0
);
";

pub fn open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| e.to_string())?;
    conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    Ok(conn)
}

pub fn seed_settings(conn: &Connection, defaults: &Settings) -> Result<(), String> {
    let existing: i64 = conn
        .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if existing > 0 {
        return Ok(());
    }
    save_settings(conn, defaults)
}

pub fn save_settings(conn: &Connection, settings: &Settings) -> Result<(), String> {
    for (key, value) in settings.pairs() {
        conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn load_settings(conn: &Connection, defaults: &Settings) -> Result<Settings, String> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (k, v) = row.map_err(|e| e.to_string())?;
        map.insert(k, v);
    }
    let text = |key: &str, fallback: &str| map.get(key).cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| fallback.to_string());
    let num = |key: &str, fallback: i64| {
        map.get(key).and_then(|s| s.parse::<i64>().ok()).unwrap_or(fallback)
    };
    let theme = text("theme", &defaults.theme);
    let theme = if matches!(theme.as_str(), "light" | "dark" | "system") { theme } else { "system".into() };
    let live_format = text("liveFormat", &defaults.live_format);
    let live_format = if matches!(live_format.as_str(), "mkv" | "mp4" | "ts" | "flv") {
        live_format
    } else {
        "mkv".into()
    };
    let live_quality = text("liveQuality", &defaults.live_quality).to_ascii_uppercase();
    let live_quality = if matches!(live_quality.as_str(), "OD" | "UHD" | "HD" | "SD" | "LD") {
        live_quality
    } else {
        "OD".into()
    };
    Ok(Settings {
        download_dir: text("downloadDir", &defaults.download_dir),
        max_concurrency: num("maxConcurrency", defaults.max_concurrency).clamp(1, 10),
        download_threads: num("downloadThreads", defaults.download_threads).clamp(1, 16),
        default_quality: text("defaultQuality", &defaults.default_quality),
        default_format: text("defaultFormat", &defaults.default_format),
        max_speed: num("maxSpeed", defaults.max_speed).max(0),
        timeout_sec: num("timeoutSec", defaults.timeout_sec).clamp(5, 300),
        max_retries: num("maxRetries", defaults.max_retries).clamp(0, 10),
        theme,
        cookie_file: map.get("cookieFile").cloned().unwrap_or_default(),
        live_dir: text("liveDir", &defaults.live_dir),
        live_format,
        live_quality,
        live_max_concurrent: num("liveMaxConcurrent", defaults.live_max_concurrent).clamp(1, 12),
        close_to_tray: map.get("closeToTray").map(|s| s != "false").unwrap_or(defaults.close_to_tray),
        ytdlp_path: map.get("ytdlpPath").cloned().unwrap_or_default(),
        ffmpeg_path: map.get("ffmpegPath").cloned().unwrap_or_default(),
    })
}

fn task_from(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        platform: row.get(3)?,
        platform_label: row.get(4)?,
        author: row.get(5)?,
        thumbnail: row.get(6)?,
        duration: row.get(7)?,
        quality: row.get(8)?,
        container: row.get(9)?,
        format_id: row.get(10)?,
        ext: row.get(11)?,
        filesize: row.get(12)?,
        downloaded_bytes: row.get(13)?,
        total_bytes: row.get(14)?,
        speed: row.get(15)?,
        eta: row.get(16)?,
        percent: row.get(17)?,
        status: TaskStatus::parse(&row.get::<_, String>(18)?),
        error: row.get(19)?,
        output_path: row.get(20)?,
        save_dir: row.get(21)?,
        progress_text: row.get(22)?,
        created_at: row.get(23)?,
        updated_at: row.get(24)?,
        started_at: row.get(25)?,
        completed_at: row.get(26)?,
    })
}

const TASK_COLS: &str = "id, url, title, platform, platform_label, author, thumbnail, duration, quality, container, format_id, ext, filesize, downloaded_bytes, total_bytes, speed, eta, percent, status, error, output_path, save_dir, progress_text, created_at, updated_at, started_at, completed_at";

pub fn load_tasks(conn: &Connection) -> Result<Vec<Task>, String> {
    let sql = format!("SELECT {TASK_COLS} FROM tasks ORDER BY created_at DESC LIMIT 500");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], task_from).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn save_task(conn: &Connection, task: &Task) -> Result<(), String> {
    conn.execute(
        "INSERT INTO tasks (
            id, url, title, platform, platform_label, author, thumbnail, duration, quality, container,
            format_id, ext, filesize, downloaded_bytes, total_bytes, speed, eta, percent, status, error,
            output_path, save_dir, progress_text, created_at, updated_at, started_at, completed_at
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
        ON CONFLICT(id) DO UPDATE SET
            url=excluded.url, title=excluded.title, platform=excluded.platform, platform_label=excluded.platform_label,
            author=excluded.author, thumbnail=excluded.thumbnail, duration=excluded.duration, quality=excluded.quality,
            container=excluded.container, format_id=excluded.format_id, ext=excluded.ext, filesize=excluded.filesize,
            downloaded_bytes=excluded.downloaded_bytes, total_bytes=excluded.total_bytes, speed=excluded.speed,
            eta=excluded.eta, percent=excluded.percent, status=excluded.status, error=excluded.error,
            output_path=excluded.output_path, save_dir=excluded.save_dir, progress_text=excluded.progress_text,
            created_at=excluded.created_at, updated_at=excluded.updated_at, started_at=excluded.started_at,
            completed_at=excluded.completed_at",
        params![
            task.id, task.url, task.title, task.platform, task.platform_label, task.author, task.thumbnail,
            task.duration, task.quality, task.container, task.format_id, task.ext, task.filesize,
            task.downloaded_bytes, task.total_bytes, task.speed, task.eta, task.percent, task.status.as_str(),
            task.error, task.output_path, task.save_dir, task.progress_text, task.created_at, task.updated_at,
            task.started_at, task.completed_at
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_task(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM tasks WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn pause_interrupted(conn: &Connection) -> Result<(), String> {
    let now = now_ms();
    conn.execute(
        "UPDATE tasks SET status = 'paused', speed = 0, progress_text = '上次未完成，已暂停，可继续', updated_at = ?1
         WHERE status IN ('downloading', 'parsing')",
        params![now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn room_from(row: &Row<'_>) -> rusqlite::Result<LiveRoom> {
    let live_on: i64 = row.get(21)?;
    Ok(LiveRoom {
        id: row.get(0)?,
        url: row.get(1)?,
        quality: row.get(2)?,
        format: row.get(3)?,
        platform: row.get(4)?,
        platform_label: row.get(5)?,
        anchor_name: row.get(6)?,
        title: row.get(7)?,
        thumbnail: row.get(8)?,
        status: RoomStatus::parse(&row.get::<_, String>(9)?),
        message: row.get(10)?,
        error: row.get(11)?,
        output_path: row.get(12)?,
        record_url: row.get(13)?,
        alt_url: row.get(14)?,
        current_recording_id: row.get(15)?,
        filesize: row.get(16)?,
        speed: row.get(17)?,
        started_at: row.get(18)?,
        created_at: row.get(19)?,
        updated_at: row.get(20)?,
        live_on: match live_on {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        },
    })
}

pub fn load_rooms(conn: &Connection) -> Result<Vec<LiveRoom>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, url, quality, format, platform, platform_label, anchor_name, title, thumbnail, status,
                    message, error, output_path, record_url, alt_url, current_recording_id, filesize, speed,
                    started_at, created_at, updated_at, live_on
             FROM live_rooms ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], room_from).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn save_room(conn: &Connection, room: &LiveRoom) -> Result<(), String> {
    let live_on = match room.live_on {
        Some(true) => 1,
        Some(false) => 0,
        None => -1,
    };
    conn.execute(
        "INSERT INTO live_rooms (
            id, url, quality, format, platform, platform_label, anchor_name, title, thumbnail, status,
            message, error, output_path, record_url, alt_url, current_recording_id, filesize, speed,
            started_at, created_at, updated_at, live_on
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
        ON CONFLICT(id) DO UPDATE SET
            url=excluded.url, quality=excluded.quality, format=excluded.format, platform=excluded.platform,
            platform_label=excluded.platform_label, anchor_name=excluded.anchor_name, title=excluded.title,
            thumbnail=excluded.thumbnail, status=excluded.status, message=excluded.message, error=excluded.error,
            output_path=excluded.output_path, record_url=excluded.record_url, alt_url=excluded.alt_url,
            current_recording_id=excluded.current_recording_id, filesize=excluded.filesize, speed=excluded.speed,
            started_at=excluded.started_at, created_at=excluded.created_at, updated_at=excluded.updated_at,
            live_on=excluded.live_on",
        params![
            room.id, room.url, room.quality, room.format, room.platform, room.platform_label, room.anchor_name,
            room.title, room.thumbnail, room.status.as_str(), room.message, room.error, room.output_path,
            room.record_url, room.alt_url, room.current_recording_id, room.filesize, room.speed, room.started_at,
            room.created_at, room.updated_at, live_on
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_room(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM live_rooms WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

fn recording_from(row: &Row<'_>) -> rusqlite::Result<LiveRecording> {
    Ok(LiveRecording {
        id: row.get(0)?,
        room_id: row.get(1)?,
        url: row.get(2)?,
        platform: row.get(3)?,
        platform_label: row.get(4)?,
        anchor_name: row.get(5)?,
        title: row.get(6)?,
        quality: row.get(7)?,
        format: row.get(8)?,
        status: RecordingStatus::parse(&row.get::<_, String>(9)?),
        error: row.get(10)?,
        output_path: row.get(11)?,
        filesize: row.get(12)?,
        duration_sec: row.get(13)?,
        started_at: row.get(14)?,
        ended_at: row.get(15)?,
        created_at: row.get(16)?,
    })
}

pub fn load_recordings(conn: &Connection) -> Result<Vec<LiveRecording>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, room_id, url, platform, platform_label, anchor_name, title, quality, format, status,
                    error, output_path, filesize, duration_sec, started_at, ended_at, created_at
             FROM live_recordings ORDER BY created_at DESC LIMIT 200",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], recording_from).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn save_recording(conn: &Connection, item: &LiveRecording) -> Result<(), String> {
    conn.execute(
        "INSERT INTO live_recordings (
            id, room_id, url, platform, platform_label, anchor_name, title, quality, format, status,
            error, output_path, filesize, duration_sec, started_at, ended_at, created_at
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
        ON CONFLICT(id) DO UPDATE SET
            room_id=excluded.room_id, url=excluded.url, platform=excluded.platform, platform_label=excluded.platform_label,
            anchor_name=excluded.anchor_name, title=excluded.title, quality=excluded.quality, format=excluded.format,
            status=excluded.status, error=excluded.error, output_path=excluded.output_path, filesize=excluded.filesize,
            duration_sec=excluded.duration_sec, started_at=excluded.started_at, ended_at=excluded.ended_at,
            created_at=excluded.created_at",
        params![
            item.id, item.room_id, item.url, item.platform, item.platform_label, item.anchor_name, item.title,
            item.quality, item.format, item.status.as_str(), item.error, item.output_path, item.filesize,
            item.duration_sec, item.started_at, item.ended_at, item.created_at
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_recording(conn: &Connection, id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM live_recordings WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn recording_by_id(conn: &Connection, id: &str) -> Result<Option<LiveRecording>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, room_id, url, platform, platform_label, anchor_name, title, quality, format, status,
                    error, output_path, filesize, duration_sec, started_at, ended_at, created_at
             FROM live_recordings WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;
    stmt.query_row(params![id], recording_from)
        .optional()
        .map_err(|e| e.to_string())
}

pub fn stats(conn: &Connection) -> Result<Stats, String> {
    let today = local_day(now_ms());
    let one = |sql: &str| -> Result<i64, String> {
        conn.query_row(sql, params![today], |row| row.get(0)).map_err(|e| e.to_string())
    };
    let today_tasks = one("SELECT COUNT(*) FROM tasks WHERE created_at > 0 AND date(created_at / 1000, 'unixepoch', 'localtime') = ?1")?;
    let completed_today = one("SELECT COUNT(*) FROM tasks WHERE status = 'completed' AND date(completed_at / 1000, 'unixepoch', 'localtime') = ?1")?;
    let failed_today = one("SELECT COUNT(*) FROM tasks WHERE status = 'failed' AND date(updated_at / 1000, 'unixepoch', 'localtime') = ?1")?;
    let downloading: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE status IN ('downloading', 'parsing')", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let total_completed: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE status = 'completed'", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let total_bytes: i64 = conn
        .query_row("SELECT COALESCE(SUM(filesize), 0) FROM tasks WHERE status = 'completed'", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let total_tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let finished: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE status IN ('completed', 'failed')", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    let success_rate = if finished == 0 { 0.0 } else { total_completed as f64 / finished as f64 };
    let mut stmt = conn
        .prepare(
            "SELECT platform, platform_label, COUNT(*), COALESCE(SUM(filesize), 0)
             FROM tasks WHERE status = 'completed' GROUP BY platform ORDER BY COUNT(*) DESC LIMIT 8",
        )
        .map_err(|e| e.to_string())?;
    let by_platform = stmt
        .query_map([], |row| {
            Ok(crate::model::PlatformStat {
                platform: row.get(0)?,
                label: row.get(1)?,
                count: row.get(2)?,
                bytes: row.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(Stats {
        today_tasks,
        completed_today,
        downloading,
        failed_today,
        total_completed,
        total_bytes,
        total_tasks,
        success_rate,
        by_platform,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_and_a_task_roundtrip() {
        let dir = std::env::temp_dir().join(format!("meng-db-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = open(&dir.join("t.sqlite")).unwrap();
        let mut defaults = Settings::default();
        defaults.download_dir = dir.join("downloads").display().to_string();
        defaults.live_dir = dir.join("lives").display().to_string();
        seed_settings(&conn, &defaults).unwrap();
        defaults.max_concurrency = 4;
        save_settings(&conn, &defaults).unwrap();
        let loaded = load_settings(&conn, &Settings::default()).unwrap();
        assert_eq!(loaded.max_concurrency, 4);
        assert_eq!(loaded.download_dir, defaults.download_dir);
        let mut task = Task::blank("https://example.com/v".into());
        task.title = "示例".into();
        task.status = TaskStatus::Completed;
        save_task(&conn, &task).unwrap();
        let tasks = load_tasks(&conn).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "示例");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
