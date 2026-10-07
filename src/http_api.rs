use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::cmd::{Cmd, IngestKind};
use crate::model::IngestResult;
use crate::tools::API_PORTS;

pub struct Incoming {
    pub url: String,
    pub action: String,
    pub title: String,
}

pub fn parse_extension_body(body: &str) -> Result<Incoming, String> {
    let value: Value = serde_json::from_str(body.trim()).map_err(|_| "请提供链接".to_string())?;
    let url = value.get("url").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if url.is_empty() || url.len() > 20_000 {
        return Err("请提供链接".into());
    }
    let action = value.get("action").and_then(|v| v.as_str()).unwrap_or("auto").to_string();
    let title = value.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok(Incoming { url, action, title })
}

pub fn parse_mobile_body(body: &str) -> Result<Incoming, String> {
    let body = body.trim().trim_start_matches('\u{feff}');
    if body.is_empty() || body.len() > 20_000 {
        return Err("请提供链接或分享文本".into());
    }
    if body.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            if let Some(text) = value.as_str() {
                return Ok(Incoming { url: decode_maybe(text), action: "live".into(), title: String::new() });
            }
            let url = value.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let text = value.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let action = value.get("action").and_then(|v| v.as_str()).unwrap_or("live");
            let raw = if !url.is_empty() { url } else { text };
            if raw.is_empty() {
                return Err("请提供链接或分享文本".into());
            }
            return Ok(Incoming {
                url: decode_maybe(raw),
                action: action.to_string(),
                title: "快捷指令提交的直播".into(),
            });
        }
    }
    if let Some(url) = form_url(body) {
        return Ok(Incoming { url: decode_maybe(&url), action: "live".into(), title: "快捷指令提交的直播".into() });
    }
    Ok(Incoming { url: decode_maybe(body), action: "live".into(), title: "快捷指令提交的直播".into() })
}

fn form_url(body: &str) -> Option<String> {
    if body.contains('\n') || !body.contains("url=") {
        return None;
    }
    for pair in body.split('&') {
        let mut parts = pair.splitn(2, '=');
        if parts.next() == Some("url") {
            return parts.next().map(|s| s.to_string());
        }
    }
    None
}

fn decode_maybe(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("http://") || lower.contains("https://") {
        return raw.to_string();
    }
    let decoded = percent_decode(raw);
    let lower = decoded.to_ascii_lowercase();
    if lower.contains("http://") || lower.contains("https://") {
        decoded
    } else {
        raw.to_string()
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn serve(tx: Sender<Cmd>) -> Option<u16> {
    for port in API_PORTS {
        match TcpListener::bind(("0.0.0.0", port)) {
            Ok(listener) => {
                log::info!("mobile and extension API on 0.0.0.0:{port}");
                thread::spawn(move || accept_loop(listener, tx));
                return Some(port);
            }
            Err(err) => log::info!("port {port} is busy: {err}"),
        }
    }
    None
}

fn accept_loop(listener: TcpListener, tx: Sender<Cmd>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let tx = tx.clone();
        thread::spawn(move || {
            if let Err(err) = handle(stream, &tx) {
                log::debug!("api connection: {err}");
            }
        });
    }
}

fn handle(mut stream: TcpStream, tx: &Sender<Cmd>) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(8))).ok();
    let peer = stream.peer_addr().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if header_end(&buf).is_some() {
                    break;
                }
                if buf.len() > 65_536 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let Some(split) = header_end(&buf) else {
        respond(&mut stream, 400, "Bad Request", r#"{"error":"请求不完整"}"#);
        return Ok(());
    };
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut body = buf[split + 4..].to_vec();
    let length = content_length(&head);
    while body.len() < length && body.len() < 65_536 {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    let mut lines = head.lines();
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
    if method == "OPTIONS" {
        respond(&mut stream, 204, "No Content", "");
        return Ok(());
    }
    let loopback = peer.ip().is_loopback();
    let mobile = method == "POST" && path == "/api/mobile/link";
    if !loopback && !mobile {
        respond(&mut stream, 403, "Forbidden", r#"{"error":"局域网只能提交手机快捷指令"}"#);
        return Ok(());
    }
    let body_text = String::from_utf8_lossy(&body).to_string();
    match (method.as_str(), path) {
        ("GET", "/api/health") | ("GET", "/api/extension/ping") => {
            let body = format!(
                "{{\"ok\":true,\"name\":\"萌下载\",\"version\":\"{}\"}}",
                env!("CARGO_PKG_VERSION")
            );
            respond(&mut stream, 200, "OK", &body);
        }
        ("POST", "/api/extension/add") | ("POST", "/api/mobile/link") => {
            let kind = if path.ends_with("mobile/link") { IngestKind::Mobile } else { IngestKind::Extension };
            let (reply_tx, reply_rx) = mpsc::channel();
            if tx
                .send(Cmd::Ingest { kind, body: body_text, reply: reply_tx })
                .is_err()
            {
                respond(&mut stream, 503, "Unavailable", r#"{"error":"下载器没有在运行"}"#);
                return Ok(());
            }
            match reply_rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(result)) => {
                    let body = serde_json::to_string(&IngestResult { kind: result.kind, message: result.message })
                        .unwrap_or_else(|_| r#"{"ok":true}"#.into());
                    let wrapped = format!("{{\"ok\":true,{}}}", body.trim_start_matches('{').trim_end_matches('}'));
                    respond(&mut stream, 201, "Created", &wrapped);
                }
                Ok(Err(err)) => {
                    let status = if err.contains("已存在") { 409 } else { 400 };
                    let reason = if status == 409 { "Conflict" } else { "Bad Request" };
                    let body = format!("{{\"error\":{}}}", serde_json::to_string(&err).unwrap_or_else(|_| "\"失败\"".into()));
                    respond(&mut stream, status, reason, &body);
                }
                Err(_) => respond(&mut stream, 504, "Gateway Timeout", r#"{"error":"处理超时"}"#),
            }
        }
        _ => respond(&mut stream, 404, "Not Found", r#"{"error":"没有这个接口"}"#),
    }
    Ok(())
}

fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n\r\n{body}",
        body.as_bytes().len()
    );
    let _ = stream.write_all(msg.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_body_accepts_raw_text_and_json() {
        let raw = parse_mobile_body("直播来了 https://live.douyin.com/123456").unwrap();
        assert!(raw.url.contains("live.douyin.com/123456"));
        assert_eq!(raw.action, "live");
        let json = parse_mobile_body(r#"{"text":"https://live.douyin.com/999","action":"live"}"#).unwrap();
        assert_eq!(json.url, "https://live.douyin.com/999");
    }

    #[test]
    fn extension_body_needs_a_url() {
        assert!(parse_extension_body(r#"{"url":"https://example.com/a","action":"download"}"#).is_ok());
        assert!(parse_extension_body("{}").is_err());
    }
}
