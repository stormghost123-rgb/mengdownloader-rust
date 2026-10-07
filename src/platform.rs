use std::sync::LazyLock;

use regex::Regex;
use url::Url;

struct Rule {
    id: &'static str,
    label: &'static str,
    re: Regex,
}

fn rule(id: &'static str, label: &'static str, pattern: &str) -> Rule {
    Rule {
        id,
        label,
        re: Regex::new(pattern).expect("platform pattern"),
    }
}

static VOD_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule("youtube", "YouTube", r"(?:youtube\.com/(?:watch\?[^ ]*v=|shorts/|embed/|live/)|youtu\.be/)"),
        rule("bilibili", "Bilibili", r"(?:bilibili\.com/video/|b23\.tv/|bilibili\.com/bangumi/|live\.bilibili\.com/)"),
        rule("vimeo", "Vimeo", r"vimeo\.com/[0-9]"),
        rule("x", "X (Twitter)", r"(?:twitter\.com/|x\.com/)[^/]+/status/[0-9]+"),
        rule("tiktok", "TikTok", r"(?:tiktok\.com/|vm\.tiktok\.com/)"),
        rule("instagram", "Instagram", r"instagram\.com/(?:reel|p|tv)/"),
        rule("douyin", "抖音", r"(?:live\.douyin\.com/|douyin\.com/(?:video|note|live)/|v\.douyin\.com/|iesdouyin\.com/)"),
        rule("kuaishou", "快手", r"(?:live\.kuaishou\.com/|kuaishou\.com/(?:short-video|f|u)/|v\.kuaishou\.com/|gifshow\.com)"),
    ]
});

static LIVE_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule("douyin", "抖音直播", r"(?:live\.douyin\.com/|douyin\.com/(?:root/)?live/|v\.douyin\.com/|iesdouyin)"),
        rule("kuaishou", "快手直播", r"(?:live\.kuaishou\.com/|v\.kuaishou\.com/|gifshow\.com|chenzhongtech)"),
        rule("bilibili", "B站直播", r"(?:live\.bilibili\.com/|b23\.tv/)"),
        rule("huya", "虎牙直播", r"huya\.com/"),
        rule("douyu", "斗鱼直播", r"douyu\.com/"),
        rule("tiktok", "TikTok Live", r"tiktok\.com/@[^/\s]+/live"),
        rule("xiaohongshu", "小红书直播", r"(?:xiaohongshu\.com/(?:hina/)?(?:livestream|live)|live\.xiaohongshu\.com|xhslink\.com)"),
        rule("youtube", "YouTube Live", r"(?:youtube\.com/(?:live/|watch\?)|youtu\.be/)"),
        rule("twitch", "Twitch", r"twitch\.tv/"),
    ]
});

#[derive(Clone, Debug)]
pub struct PlatformInfo {
    pub id: String,
    pub label: String,
}

pub fn detect_platform(url: &str) -> PlatformInfo {
    for rule in VOD_RULES.iter() {
        if rule.re.is_match(url) {
            return PlatformInfo { id: rule.id.into(), label: rule.label.into() };
        }
    }
    PlatformInfo { id: "generic".into(), label: "其他站点".into() }
}

pub fn detect_live(url: &str) -> PlatformInfo {
    let s = url.trim();
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 6 {
        return PlatformInfo { id: "douyin".into(), label: "抖音直播".into() };
    }
    for rule in LIVE_RULES.iter() {
        if rule.re.is_match(s) {
            return PlatformInfo { id: rule.id.into(), label: rule.label.into() };
        }
    }
    PlatformInfo { id: "generic".into(), label: "其他直播".into() }
}

pub fn looks_like_live(url: &str) -> bool {
    let info = detect_live(url);
    if info.id == "generic" {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains("/video") || lower.contains("/note") || lower.contains("bilibili.com/video") || lower.contains("watch?v=") {
        return false;
    }
    true
}

/// Share text is often "caption + url + caption". Pull the first http(s) URL out of it.
pub fn extract_url(text: &str) -> Option<String> {
    let s = text.trim().trim_start_matches('\u{feff}');
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let start = lower.find("https://").or_else(|| lower.find("http://"))?;
    let tail = &s[start..];
    let end = tail
        .find(|c: char| c.is_whitespace() || "\"'<>，。！？、；：（）【】《》".contains(c))
        .unwrap_or(tail.len());
    let url = tail[..end].trim_end_matches(|c: char| ".,;:!?)]\"'”’".contains(c));
    if url.len() < 8 {
        None
    } else {
        Some(url.to_string())
    }
}

pub fn classify(action: &str, url: &str) -> &'static str {
    match action {
        "live" => "live",
        "download" => "download",
        _ => {
            if looks_like_live(url) {
                "live"
            } else {
                "download"
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DouyinIds {
    pub web_rid: String,
    pub room_id: String,
}

pub fn is_internal_room_id(id: &str) -> bool {
    id.len() >= 16 && id.chars().all(|c| c.is_ascii_digit())
}

fn assign_id(out: &mut DouyinIds, id: &str) {
    if id.len() < 6 || !id.chars().all(|c| c.is_ascii_digit()) {
        return;
    }
    if is_internal_room_id(id) {
        if out.room_id.is_empty() {
            out.room_id = id.to_string();
        }
    } else if out.web_rid.is_empty() {
        out.web_rid = id.to_string();
    }
}

fn digits_after(text: &str, marker: &str) -> String {
    let Some(pos) = text.find(marker) else {
        return String::new();
    };
    text[pos + marker.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect()
}

pub fn extract_douyin_ids(input: &str) -> DouyinIds {
    let mut out = DouyinIds::default();
    assign_id(&mut out, &digits_after(input, "live.douyin.com/"));
    // The website share page is www.douyin.com/root/live/{web_rid}, not live.douyin.com.
    if out.web_rid.is_empty() && out.room_id.is_empty() {
        for marker in ["douyin.com/root/live/", "douyin.com/follow/live/", "douyin.com/live/"] {
            assign_id(&mut out, &digits_after(input, marker));
            if !out.web_rid.is_empty() || !out.room_id.is_empty() {
                break;
            }
        }
    }
    let reflow = digits_after(input, "reflow/");
    if is_internal_room_id(&reflow) {
        out.room_id = reflow;
    }
    for key in ["web_rid=", "web_rid\"", "room_id=", "room_id_str=", "roomId="] {
        if let Some(pos) = input.find(key) {
            let rest = input[pos + key.len()..].trim_start_matches(['"', ':', ' ', '=']);
            let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            assign_id(&mut out, &id);
        }
    }
    if out.web_rid.is_empty() && out.room_id.is_empty() {
        let only: String = input.chars().take_while(|c| c.is_ascii_digit()).collect();
        if only.len() == input.trim().len() {
            assign_id(&mut out, &only);
        }
    }
    out
}

pub fn ids_from_html(html: &str) -> DouyinIds {
    let mut out = DouyinIds::default();
    let slice = if html.len() > 400_000 { &html[..400_000] } else { html };
    if let Some(id) = json_string_digits(slice, "web_rid", 6, 15) {
        out.web_rid = id;
    }
    if let Some(id) = json_string_digits(slice, "room_id_str", 16, 24)
        .or_else(|| json_string_digits(slice, "room_id", 16, 24))
        .or_else(|| json_string_digits(slice, "roomId", 16, 24))
    {
        out.room_id = id;
    }
    let reflow = digits_after(slice, "reflow/");
    if out.room_id.is_empty() && is_internal_room_id(&reflow) {
        out.room_id = reflow;
    }
    out
}

fn json_string_digits(text: &str, key: &str, min: usize, max: usize) -> Option<String> {
    let pat = format!("\"{key}\"");
    let mut from = 0;
    while let Some(rel) = text[from..].find(&pat) {
        let start = from + rel + pat.len();
        let rest = text[start..].trim_start_matches([' ', ':', '"']);
        let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if (min..=max).contains(&id.len()) {
            return Some(id);
        }
        from = start.max(from + 1);
    }
    None
}

pub fn normalize_live_url(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 6 {
        return format!("https://live.douyin.com/{s}");
    }
    let extracted = extract_url(s).unwrap_or_else(|| s.to_string());
    let with_scheme = if extracted.contains("://") {
        extracted
    } else {
        format!("https://{extracted}")
    };
    let Ok(mut parsed) = Url::parse(&with_scheme) else {
        return with_scheme.split('?').next().unwrap_or(&with_scheme).to_string();
    };
    parsed.set_fragment(None);
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if host == "v.douyin.com" || host.ends_with(".v.douyin.com") {
        return parsed.to_string();
    }
    if host.contains("douyin") || host.contains("iesdouyin") {
        let ids = extract_douyin_ids(parsed.as_str());
        if !ids.web_rid.is_empty() {
            return format!("https://live.douyin.com/{}", ids.web_rid);
        }
        if !ids.room_id.is_empty() && !parsed.as_str().contains("reflow") && !host.contains("v.douyin") {
            return format!("https://live.douyin.com/{}", ids.room_id);
        }
    }
    parsed.to_string()
}

pub fn bilibili_room_id(url: &str) -> String {
    digits_after(url, "live.bilibili.com/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_a_url_out_of_share_text() {
        let text = "看看这个 https://www.bilibili.com/video/BV1GJ411x7h7 好好看";
        assert_eq!(
            extract_url(text).as_deref(),
            Some("https://www.bilibili.com/video/BV1GJ411x7h7")
        );
    }

    #[test]
    fn douyin_web_rid_and_internal_room_stay_apart() {
        let ids = extract_douyin_ids("https://live.douyin.com/456321954819");
        assert_eq!(ids.web_rid, "456321954819");
        assert!(ids.room_id.is_empty());
        let share = ids_from_html(r#"{"room_id":"7123456789012345678","web_rid":"901741491458"}"#);
        assert_eq!(share.web_rid, "901741491458");
        assert_eq!(share.room_id, "7123456789012345678");
    }

    #[test]
    fn douyin_website_live_path_is_a_web_rid() {
        let url = "https://www.douyin.com/root/live/392955140700?anchor_id=82036446462&r_id=f_392955140700";
        let ids = extract_douyin_ids(url);
        assert_eq!(ids.web_rid, "392955140700");
        assert!(ids.room_id.is_empty());
        assert_eq!(normalize_live_url(url), "https://live.douyin.com/392955140700");
    }

    fn a_plain_room_number_becomes_a_douyin_live_url() {
        assert_eq!(normalize_live_url("456321954819"), "https://live.douyin.com/456321954819");
        assert!(looks_like_live("https://live.douyin.com/456321954819"));
        assert!(!looks_like_live("https://www.douyin.com/video/1234567890123456789"));
    }
}
