use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde_json::Value;

use crate::cmd::{CancelFlag, Cmd};
use crate::model::{safe_name, ResolvedStream};
use crate::platform::{self, DouyinIds};
use crate::tools::{self, HTTP_UA};
use crate::ytdlp;

const MOBILE_UA: &str = "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Mobile Safari/537.36";

struct HttpText {
    text: String,
    final_url: String,
    cookie: String,
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(HTTP_UA)
        .redirect(Policy::limited(8))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

fn cookie_pairs(resp: &reqwest::blocking::Response) -> String {
    let mut pairs = Vec::new();
    for value in resp.headers().get_all("set-cookie") {
        let Ok(text) = value.to_str() else { continue };
        let pair = text.split(';').next().unwrap_or("").trim();
        let lower = pair.to_ascii_lowercase();
        if lower.starts_with("ttwid=") || lower.starts_with("uifid") {
            pairs.push(pair.to_string());
        }
    }
    pairs.join("; ")
}

fn http_get(client: &Client, url: &str, headers: &[(&str, &str)]) -> Result<HttpText, String> {
    let mut req = client.get(url);
    for (key, value) in headers {
        req = req.header(*key, *value);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    let final_url = resp.url().to_string();
    let cookie = cookie_pairs(&resp);
    let text = resp.text().map_err(|e| e.to_string())?;
    Ok(HttpText { text, final_url, cookie })
}

fn cookie_cache() -> &'static Mutex<Option<(String, Instant)>> {
    static CACHE: Mutex<Option<(String, Instant)>> = Mutex::new(None);
    &CACHE
}

fn cached_cookie() -> String {
    let guard = cookie_cache().lock().unwrap_or_else(|e| e.into_inner());
    match &*guard {
        Some((value, at)) if at.elapsed() < Duration::from_secs(30 * 60) && value.contains("ttwid=") => value.clone(),
        _ => String::new(),
    }
}

fn store_cookie(cookie: &str) {
    if !cookie.contains("ttwid=") {
        return;
    }
    *cookie_cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((cookie.to_string(), Instant::now()));
}

fn douyin_cookie(client: &Client, force: bool) -> String {
    if !force {
        let cached = cached_cookie();
        if !cached.is_empty() {
            return cached;
        }
    }
    let Ok(resp) = http_get(client, "https://live.douyin.com/", &[("Accept", "text/html")]) else {
        return cached_cookie();
    };
    if resp.cookie.contains("ttwid=") {
        store_cookie(&resp.cookie);
        return resp.cookie;
    }
    cached_cookie()
}

fn enter_url(web_rid: &str) -> String {
    format!(
        "https://live.douyin.com/webcast/room/web/enter/?aid=6383&app_name=douyin_web&live_id=1&device_platform=web&language=zh-CN&enter_from=web_live&cookie_enabled=true&screen_width=1920&screen_height=1080&browser_language=zh-CN&browser_platform=Win32&browser_name=Chrome&browser_version=122.0.0.0&web_rid={}",
        urlencoding_web_rid(web_rid)
    )
}

fn urlencoding_web_rid(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn resolve(raw: &str, quality: &str, ytdlp: Option<&Path>) -> ResolvedStream {
    let url = platform::normalize_live_url(raw);
    let detected = platform::detect_live(&url);
    let mut info = if detected.id == "douyin" {
        resolve_douyin(&url, quality).unwrap_or_else(|err| {
            let mut info = ResolvedStream::empty(&detected.id, &detected.label);
            info.error = err;
            info
        })
    } else if detected.id == "bilibili" {
        resolve_bilibili(&url, quality).unwrap_or_else(|err| {
            let mut info = ResolvedStream::empty(&detected.id, &detected.label);
            info.error = err;
            info
        })
    } else {
        let mut info = ResolvedStream::empty(&detected.id, &detected.label);
        info.error = "等待 yt-dlp 解析".into();
        info
    };
    if info.record_url.is_empty() && !info.offline {
        if let Some(bin) = ytdlp {
            match ytdlp::stream_url(bin, &url) {
                Ok(stream) => {
                    info.record_url = stream;
                    info.is_live = true;
                    info.error.clear();
                    info.source = "ytdlp".into();
                    if info.title.is_empty() {
                        info.title = detected.label.clone();
                    }
                }
                Err(err) => {
                    if info.error.is_empty() || info.error == "等待 yt-dlp 解析" {
                        info.error = err;
                    }
                }
            }
        } else if info.error.is_empty() || info.error == "等待 yt-dlp 解析" {
            info.error = "没有拿到直播流，也没有找到 yt-dlp".into();
        }
    }
    if info.platform.is_empty() {
        info.platform = detected.id;
        info.platform_label = detected.label;
    }
    info
}

fn resolve_douyin(url: &str, quality: &str) -> Result<ResolvedStream, String> {
    let client = client()?;
    let detected = platform::detect_live(url);
    let mut info = ResolvedStream::empty(&detected.id, &detected.label);
    info.source = "native".into();
    let mut ids = platform::extract_douyin_ids(url);
    if ids.web_rid.is_empty() && (url.contains("v.douyin.com") || url.contains("reflow") || url.contains("iesdouyin")) {
        if let Ok(found) = follow_share(&client, url) {
            if !found.web_rid.is_empty() {
                ids.web_rid = found.web_rid;
            }
            if !found.room_id.is_empty() {
                ids.room_id = found.room_id;
            }
        }
    }
    let mut cookie = douyin_cookie(&client, false);
    if !ids.web_rid.is_empty() {
        if let Some(entered) = fetch_enter(&client, &ids.web_rid, &cookie) {
            cookie = entered.1;
            apply_douyin(&mut info, &entered.0, quality, "douyin webcast enter");
            if info.is_live || info.offline {
                return Ok(info);
            }
        }
    }
    if !ids.room_id.is_empty() {
        if let Some(room) = reflow_room(&client, &ids.room_id) {
            let web = room
                .pointer("/owner/web_rid")
                .or_else(|| room.pointer("/owner/web_rid_str"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if web.len() >= 6 && web.len() < 16 && web.chars().all(|c| c.is_ascii_digit()) {
                if let Some(entered) = fetch_enter(&client, web, &cookie) {
                    apply_douyin(&mut info, &entered.0, quality, "douyin phone-share reflow");
                    if info.is_live || info.offline {
                        return Ok(info);
                    }
                }
            }
            apply_room_value(&mut info, &room, room.get("owner").unwrap_or(&Value::Null), quality, "douyin reflow");
            if info.is_live || info.offline {
                return Ok(info);
            }
        }
    }
    if info.error.is_empty() {
        info.error = if ids.web_rid.is_empty() && ids.room_id.is_empty() {
            "没有从这段文字里识别出抖音直播间。请粘贴直播间链接或房间号。".into()
        } else {
            "没有拿到抖音直播流。直播间可能未开播。".into()
        };
    }
    Ok(info)
}

fn follow_share(client: &Client, url: &str) -> Result<DouyinIds, String> {
    let desktop = http_get(client, url, &[("Referer", "https://www.douyin.com/")])?;
    let mut ids = platform::extract_douyin_ids(&desktop.final_url);
    let from_html = platform::ids_from_html(&desktop.text);
    if ids.web_rid.is_empty() {
        ids.web_rid = from_html.web_rid;
    }
    if ids.room_id.is_empty() {
        ids.room_id = from_html.room_id;
    }
    if !ids.web_rid.is_empty() || !ids.room_id.is_empty() {
        return Ok(ids);
    }
    let mobile = http_get(
        client,
        url,
        &[("Referer", "https://www.douyin.com/"), ("User-Agent", MOBILE_UA)],
    )?;
    let mut ids = platform::extract_douyin_ids(&mobile.final_url);
    let from_html = platform::ids_from_html(&mobile.text);
    if ids.web_rid.is_empty() {
        ids.web_rid = from_html.web_rid;
    }
    if ids.room_id.is_empty() {
        ids.room_id = from_html.room_id;
    }
    Ok(ids)
}

fn fetch_enter(client: &Client, web_rid: &str, cookie: &str) -> Option<(Value, String)> {
    let mut current = cookie.to_string();
    let referer = format!("https://live.douyin.com/{web_rid}");
    let mut response = http_get(
        client,
        &enter_url(web_rid),
        &[("Referer", referer.as_str()), ("Cookie", current.as_str())],
    )
    .ok()?;
    if response.text.trim().is_empty() {
        current = douyin_cookie(client, true);
        response = http_get(
            client,
            &enter_url(web_rid),
            &[("Referer", referer.as_str()), ("Cookie", current.as_str())],
        )
        .ok()?;
    }
    if !response.cookie.is_empty() {
        store_cookie(&response.cookie);
        if current.is_empty() {
            current = response.cookie;
        }
    }
    let json = serde_json::from_str(&response.text).ok()?;
    Some((json, current))
}

fn reflow_room(client: &Client, room_id: &str) -> Option<Value> {
    let url = format!(
        "https://webcast.amemv.com/webcast/room/reflow/info/?type_id=0&live_id=1&room_id={room_id}&app_id=1128"
    );
    let response = http_get(
        client,
        &url,
        &[
            ("Referer", "https://webcast.amemv.com/"),
            ("Origin", "https://webcast.amemv.com"),
            ("User-Agent", MOBILE_UA),
        ],
    )
    .ok()?;
    let json: Value = serde_json::from_str(&response.text).ok()?;
    json.pointer("/data/room").cloned()
}

fn apply_douyin(info: &mut ResolvedStream, json: &Value, quality: &str, diagnostics: &str) {
    let room = json
        .pointer("/data/data/0")
        .or_else(|| json.pointer("/data/room"))
        .unwrap_or(&Value::Null);
    let user = json.pointer("/data/user").unwrap_or(&Value::Null);
    apply_room_value(info, room, user, quality, diagnostics);
}

fn apply_room_value(info: &mut ResolvedStream, room: &Value, user: &Value, quality: &str, diagnostics: &str) {
    if !room.is_object() && !user.is_object() {
        return;
    }
    let stream = room.get("stream_url").cloned().unwrap_or(Value::Null);
    let (flv, hls) = pulls_from_stream(&stream);
    let flv_url = pick_quality(&flv, quality);
    let hls_url = {
        let picked = pick_quality(&hls, quality);
        if picked.is_empty() {
            stream.get("hls_pull_url").and_then(|v| v.as_str()).unwrap_or("").to_string()
        } else {
            picked
        }
    };
    let record = pick_sharper(&flv_url, &hls_url);
    let owner = room.get("owner").unwrap_or(&Value::Null);
    info.anchor_name = first_text(&[
        user.get("nickname"),
        owner.get("nickname"),
        room.get("nickname"),
    ]);
    if info.anchor_name.is_empty() {
        info.anchor_name = info.platform_label.clone();
    }
    let title = first_text(&[room.get("title")]);
    if !title.is_empty() {
        info.title = title;
    } else if info.title.is_empty() {
        info.title = "抖音直播".into();
    }
    info.source = "native".into();
    let status = room.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
    // Status 2 is live. Status 4 is offline, and reflow can keep a stale FLV after the room ends.
    if !record.is_empty() && status != 4 {
        info.is_live = true;
        info.offline = false;
        info.record_url = record.clone();
        info.alt_url = if record == flv_url { hls_url } else { flv_url };
        if info.alt_url == info.record_url {
            info.alt_url.clear();
        }
        info.error.clear();
        return;
    }
    if status == 2 {
        info.is_live = true;
        info.offline = false;
        info.error = format!("已开播但未拿到流地址（{diagnostics}）");
        return;
    }
    if status == 4 || !info.anchor_name.is_empty() {
        info.is_live = false;
        info.offline = status == 4 || info.anchor_name != info.platform_label;
        info.record_url.clear();
        if info.offline {
            info.error = "直播间未开播或已下播".into();
        }
    }
}

fn first_text(values: &[Option<&Value>]) -> String {
    values.iter().find_map(|v| v.and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())).unwrap_or_default()
}

pub fn pulls_from_stream(stream: &Value) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut flv = BTreeMap::new();
    let mut hls = BTreeMap::new();
    let mut raw = stream.pointer("/live_core_sdk_data/pull_data/stream_data").cloned();
    if let Some(Value::String(text)) = &raw {
        raw = serde_json::from_str(text).ok();
    }
    if let Some(data) = raw.as_ref().and_then(|v| v.get("data")).and_then(|v| v.as_object()) {
        for (name, node) in data {
            let Some(main) = node.get("main") else { continue };
            let keys = alias(name);
            if let Some(url) = main.get("flv").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                for key in &keys {
                    flv.entry(key.clone()).or_insert_with(|| url.to_string());
                }
            }
            if let Some(url) = main.get("hls").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                for key in &keys {
                    hls.entry(key.clone()).or_insert_with(|| url.to_string());
                }
            }
        }
    }
    if let Some(map) = stream.get("flv_pull_url").and_then(|v| v.as_object()) {
        for (key, value) in map {
            if let Some(url) = value.as_str().filter(|s| !s.is_empty()) {
                flv.entry(key.clone()).or_insert_with(|| url.to_string());
            }
        }
    }
    if let Some(map) = stream.get("hls_pull_url_map").and_then(|v| v.as_object()) {
        for (key, value) in map {
            if let Some(url) = value.as_str().filter(|s| !s.is_empty()) {
                hls.entry(key.clone()).or_insert_with(|| url.to_string());
            }
        }
    }
    (flv, hls)
}

fn alias(name: &str) -> Vec<String> {
    match name.to_ascii_lowercase().as_str() {
        "origin" | "or4" => vec!["ORIGIN".into(), "OR4".into()],
        "uhd" => vec!["UHD".into(), "FULL_HD1".into()],
        "hd" | "md" => vec!["HD1".into(), "HD".into()],
        "sd" => vec!["SD1".into(), "SD".into()],
        "ld" => vec!["SD2".into(), "LD".into()],
        other => vec![other.to_ascii_uppercase()],
    }
}

pub fn pick_quality(map: &BTreeMap<String, String>, quality: &str) -> String {
    let order: &[&str] = match quality {
        "UHD" => &["FULL_HD1", "FULL_HD", "ORIGIN", "HD1", "HD", "SD1", "SD2"],
        "HD" => &["HD1", "HD", "FULL_HD1", "SD1", "SD2"],
        "SD" => &["SD1", "SD", "HD1", "SD2"],
        "LD" => &["SD2", "LD", "SD1", "HD1"],
        _ => &["ORIGIN", "origin", "OR4", "FULL_HD1", "FULL_HD", "HD1", "HD", "SD1", "SD2"],
    };
    for key in order {
        if let Some(url) = map.get(*key) {
            return url.clone();
        }
    }
    map.values().next().cloned().unwrap_or_default()
}

pub fn score_pull(url: &str) -> i32 {
    if url.is_empty() {
        return 0;
    }
    let lower = url.to_ascii_lowercase();
    let mut score = 1;
    if lower.contains("_or4") || lower.contains("/origin") || lower.contains("_origin") {
        score += 120;
    }
    if lower.contains(".flv") {
        score += 40;
    }
    if lower.contains("full_hd1") || lower.contains("uhd") {
        score += 15;
    }
    if (lower.contains("index.m3u8") || lower.contains("playlist.m3u8")) && !lower.contains("origin") && !lower.contains("_or4") {
        score -= 50;
    }
    score
}

fn pick_sharper(a: &str, b: &str) -> String {
    if a.is_empty() {
        return b.to_string();
    }
    if b.is_empty() || score_pull(a) >= score_pull(b) {
        a.to_string()
    } else {
        b.to_string()
    }
}

fn resolve_bilibili(url: &str, quality: &str) -> Result<ResolvedStream, String> {
    let client = client()?;
    let detected = platform::detect_live(url);
    let mut info = ResolvedStream::empty(&detected.id, &detected.label);
    info.source = "native".into();
    let room_id = platform::bilibili_room_id(url);
    if room_id.is_empty() {
        info.error = "没有找到 B 站房间号".into();
        return Ok(info);
    }
    let meta_url = format!("https://api.live.bilibili.com/xlive/web-room/v1/index/getInfoByRoom?room_id={room_id}");
    let meta = http_get(&client, &meta_url, &[("Referer", "https://live.bilibili.com/")])?;
    let meta_json: Value = serde_json::from_str(&meta.text).map_err(|_| "B 站房间信息无法解析".to_string())?;
    let real_id = meta_json
        .pointer("/data/room_info/room_id")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or(room_id);
    info.title = meta_json.pointer("/data/room_info/title").and_then(|v| v.as_str()).unwrap_or("B站直播").to_string();
    let uname = meta_json
        .pointer("/data/anchor_info/base_info/uname")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    info.anchor_name = if uname.is_empty() { info.platform_label.clone() } else { uname };
    let live_status = meta_json.pointer("/data/room_info/live_status").and_then(|v| v.as_i64()).unwrap_or(0);
    if live_status != 1 {
        info.offline = true;
        info.error = "直播间未开播或已下播".into();
        return Ok(info);
    }
    let qn = match quality {
        "UHD" => 400,
        "HD" => 250,
        "SD" => 150,
        "LD" => 80,
        _ => 10000,
    };
    let play = format!(
        "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id={real_id}&no_playurl=0&mask=0&qn={qn}&platform=web&protocol=0,1&format=0,1,2&codec=0,1&dolby=5&panorama=1"
    );
    let play_resp = http_get(&client, &play, &[("Referer", "https://live.bilibili.com/")])?;
    let play_json: Value = serde_json::from_str(&play_resp.text).map_err(|_| "B 站播放地址无法解析".to_string())?;
    let mut flv = String::new();
    let mut hls = String::new();
    if let Some(streams) = play_json.pointer("/data/playurl_info/playurl/stream").and_then(|v| v.as_array()) {
        for stream in streams {
            let Some(formats) = stream.get("format").and_then(|v| v.as_array()) else { continue };
            for fmt in formats {
                let name = fmt.get("format_name").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
                let Some(codecs) = fmt.get("codec").and_then(|v| v.as_array()) else { continue };
                for codec in codecs {
                    let base = codec.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
                    let info0 = codec.pointer("/url_info/0");
                    let host = info0.and_then(|v| v.get("host")).and_then(|v| v.as_str()).unwrap_or("");
                    let extra = info0.and_then(|v| v.get("extra")).and_then(|v| v.as_str()).unwrap_or("");
                    if base.is_empty() || host.is_empty() {
                        continue;
                    }
                    let full = format!("{host}{base}{extra}");
                    if name == "flv" && flv.is_empty() {
                        flv = full;
                    } else if (name == "ts" || name == "fmp4" || name.contains("hls")) && hls.is_empty() {
                        hls = full;
                    } else if flv.is_empty() && hls.is_empty() {
                        flv = full;
                    }
                }
            }
        }
    }
    info.record_url = if !flv.is_empty() { flv.clone() } else { hls.clone() };
    info.alt_url = if !flv.is_empty() && !hls.is_empty() { hls } else { String::new() };
    info.is_live = !info.record_url.is_empty();
    if info.record_url.is_empty() {
        info.error = "已开播但未拿到流地址".into();
    }
    Ok(info)
}

pub fn record_args(record_url: &str, platform: &str, capture_path: &Path) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-nostats".into(),
        "-y".into(),
        "-fflags".into(),
        "+genpts+flush_packets".into(),
        "-analyzeduration".into(),
        "8M".into(),
        "-probesize".into(),
        "8M".into(),
    ];
    if record_url.starts_with("http://") || record_url.starts_with("https://") {
        args.extend([
            "-reconnect".into(),
            "1".into(),
            "-reconnect_streamed".into(),
            "1".into(),
            "-reconnect_on_network_error".into(),
            "1".into(),
            "-reconnect_on_http_error".into(),
            "500,502,503,504".into(),
            "-reconnect_delay_max".into(),
            "30".into(),
            "-rw_timeout".into(),
            "20000000".into(),
            "-headers".into(),
            tools::stream_headers(platform),
            "-user_agent".into(),
            HTTP_UA.into(),
        ]);
    }
    args.extend([
        "-i".into(),
        record_url.to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-f".into(),
        "mpegts".into(),
        capture_path.display().to_string(),
    ]);
    args
}

pub fn run_record(
    tx: Sender<Cmd>,
    id: String,
    page_url: String,
    quality: String,
    container: String,
    platform_hint: String,
    live_dir: PathBuf,
    ytdlp: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
    flag: Arc<CancelFlag>,
) {
    let _ = tx.send(Cmd::LiveMeta {
        id: id.clone(),
        is_live: false,
        offline: false,
        record_url: String::new(),
        alt_url: String::new(),
        anchor: String::new(),
        title: String::new(),
        error: String::new(),
        source: "resolving".into(),
    });
    let resolved = resolve(&page_url, &quality, ytdlp.as_deref());
    let _ = tx.send(Cmd::LiveMeta {
        id: id.clone(),
        is_live: resolved.is_live,
        offline: resolved.offline,
        record_url: resolved.record_url.clone(),
        alt_url: resolved.alt_url.clone(),
        anchor: resolved.anchor_name.clone(),
        title: resolved.title.clone(),
        error: resolved.error.clone(),
        source: resolved.source.clone(),
    });
    if flag.cancelled() || resolved.record_url.is_empty() {
        let _ = tx.send(Cmd::LiveExit {
            id,
            capture_path: String::new(),
            final_path: String::new(),
            size: 0,
            duration: 0,
            error: if flag.cancelled() { String::new() } else { resolved.error },
            cancelled: flag.cancelled(),
            remuxed: false,
        });
        return;
    }
    let Some(ffmpeg_bin) = ffmpeg else {
        let _ = tx.send(Cmd::LiveExit {
            id,
            capture_path: String::new(),
            final_path: String::new(),
            size: 0,
            duration: 0,
            error: "未找到 ffmpeg。把 ffmpeg.exe 放到 tools 目录，或沿用 N:\\deep\\tools。".into(),
            cancelled: false,
            remuxed: false,
        });
        return;
    };
    let _ = std::fs::create_dir_all(&live_dir);
    let stamp = crate::model::now_ms();
    let stem = safe_name(&format!(
        "{}_{}_{stamp}",
        if resolved.anchor_name.is_empty() { platform_hint.as_str() } else { resolved.anchor_name.as_str() },
        if resolved.title.is_empty() { "live" } else { resolved.title.as_str() }
    ));
    let capture = live_dir.join(format!("{stem}.part.ts"));
    let _ = tx.send(Cmd::LiveBegan { id: id.clone(), capture_path: capture.display().to_string() });
    let platform = if resolved.platform.is_empty() { platform_hint } else { resolved.platform };
    let code = spawn_ffmpeg(&ffmpeg_bin, &resolved.record_url, &platform, &capture, &flag, &tx, &id);
    let mut size = tools::file_size(&capture);
    if code != 0 && size < 64 * 1024 && !resolved.alt_url.is_empty() && !flag.cancelled() {
        let alt_code = spawn_ffmpeg(&ffmpeg_bin, &resolved.alt_url, &platform, &capture, &flag, &tx, &id);
        size = tools::file_size(&capture);
        if alt_code == 0 {
            let _ = code;
        }
    }
    let (final_path, remuxed, duration) = finish_file(&ffmpeg_bin, ffprobe.as_deref(), &capture, &live_dir, &stem, &container, size);
    let error = if flag.cancelled() || size > 0 {
        String::new()
    } else if resolved.error.is_empty() {
        "录制失败，没有写入文件".into()
    } else {
        resolved.error
    };
    let _ = tx.send(Cmd::LiveExit {
        id,
        capture_path: capture.display().to_string(),
        final_path,
        size,
        duration,
        error,
        cancelled: flag.cancelled(),
        remuxed,
    });
}

fn spawn_ffmpeg(bin: &Path, url: &str, platform: &str, capture: &Path, flag: &CancelFlag, tx: &Sender<Cmd>, id: &str) -> i32 {
    let args = record_args(url, platform, capture);
    let mut cmd = Command::new(bin);
    cmd.args(&args);
    tools::hidden(&mut cmd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return -1,
    };
    flag.set_pid(child.id());
    if flag.cancelled() {
        tools::kill_pid(child.id());
    }
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().take(200) {
                if line.is_err() {
                    break;
                }
            }
        });
    }
    let started = Instant::now();
    let mut last_size = tools::file_size(capture);
    let mut last_at = Instant::now();
    loop {
        if flag.cancelled() {
            tools::kill_pid(child.id());
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(-1),
            Ok(None) => {
                let size = tools::file_size(capture);
                let speed = if last_at.elapsed().as_secs_f64() > 0.2 {
                    (size.saturating_sub(last_size) as f64) / last_at.elapsed().as_secs_f64()
                } else {
                    0.0
                };
                last_size = size;
                last_at = Instant::now();
                let _ = tx.send(Cmd::LiveProgress { id: id.to_string(), size, speed });
                thread::sleep(Duration::from_millis(700));
            }
            Err(_) => return -1,
        }
        if started.elapsed() > Duration::from_secs(60 * 60 * 18) {
            tools::kill_pid(child.id());
        }
    }
}

fn finish_file(ffmpeg: &Path, ffprobe: Option<&Path>, capture: &Path, dir: &Path, stem: &str, container: &str, size: u64) -> (String, bool, i64) {
    if size == 0 || !capture.exists() {
        return (String::new(), false, 0);
    }
    let ext = match container {
        "mp4" => "mp4",
        "flv" => "flv",
        "ts" => "ts",
        _ => "mkv",
    };
    if ext == "ts" {
        let dest = dir.join(format!("{stem}.ts"));
        if capture != dest.as_path() {
            let _ = std::fs::rename(capture, &dest);
        }
        let duration = ffprobe.and_then(|bin| probe_duration(bin, &dest)).unwrap_or(0);
        return (dest.display().to_string(), true, duration);
    }
    let dest = dir.join(format!("{stem}.{ext}"));
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        &capture.display().to_string(),
        "-c",
        "copy",
        &dest.display().to_string(),
    ]);
    tools::hidden(&mut cmd);
    let remuxed = cmd.status().map(|s| s.success() && dest.exists() && tools::file_size(&dest) > 0).unwrap_or(false);
    if remuxed {
        let duration = ffprobe.and_then(|bin| probe_duration(bin, &dest)).unwrap_or(0);
        (dest.display().to_string(), true, duration)
    } else {
        (capture.display().to_string(), false, 0)
    }
}

pub fn probe_duration(ffprobe: &Path, file: &Path) -> Option<i64> {
    let mut cmd = Command::new(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        &file.display().to_string(),
    ]);
    tools::hidden(&mut cmd);
    let output = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let secs = text.trim().parse::<f64>().ok()?;
    if secs.is_finite() && secs > 0.0 { Some(secs as i64) } else { None }
}

pub fn preview(ffplay: &Path, url: &str, platform: &str) -> Result<(), String> {
    let mut cmd = Command::new(ffplay);
    cmd.args([
        "-window_title",
        "萌下载预览",
        "-x",
        "960",
        "-y",
        "540",
        "-headers",
        &tools::stream_headers(platform),
        "-user_agent",
        HTTP_UA,
        "-i",
        url,
    ]);
    tools::hidden(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| format!("无法打开 ffplay：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_flv_wins_the_requested_quality() {
        let mut flv = BTreeMap::new();
        flv.insert("ORIGIN".into(), "https://cdn/origin.flv".into());
        flv.insert("HD1".into(), "https://cdn/hd.flv".into());
        assert!(pick_quality(&flv, "OD").contains("origin"));
        assert!(pick_quality(&flv, "HD").contains("hd"));
        assert!(score_pull("https://cdn/stream_or4.flv") > score_pull("https://cdn/index.m3u8"));
    }

    #[test]
    fn sdk_pull_data_is_read_from_a_string_payload() {
        let stream = serde_json::json!({
            "live_core_sdk_data": {
                "pull_data": {
                    "stream_data": "{\"data\":{\"origin\":{\"main\":{\"flv\":\"https://cdn/a.flv\",\"hls\":\"https://cdn/a.m3u8\"}}}}"
                }
            }
        });
        let (flv, hls) = pulls_from_stream(&stream);
        assert_eq!(flv.get("ORIGIN").map(String::as_str), Some("https://cdn/a.flv"));
        assert_eq!(hls.get("OR4").map(String::as_str), Some("https://cdn/a.m3u8"));
    }
}
