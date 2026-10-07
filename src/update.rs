//! Check GitHub releases and replace the running Windows executable.
//!
//! The packaged asset must be named `mengdownloader.exe`. The green zip is for
//! people; the updater downloads that exe directly.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const OWNER: &str = "stormghost123-rgb";
const REPO: &str = "mengdownloader-rust";
const ASSET: &str = "mengdownloader.exe";
const MAX_BYTES: u64 = 80 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes: String,
    pub asset_url: String,
    pub asset_size: u64,
}

#[derive(Clone, Debug)]
pub enum UpdateReport {
    None,
    Current,
    Available(ReleaseInfo),
    Failed(String),
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    let latest = version_parts(latest);
    let current = version_parts(current);
    if latest.is_empty() {
        return false;
    }
    let len = latest.len().max(current.len());
    for i in 0..len {
        let next = *latest.get(i).unwrap_or(&0);
        let have = *current.get(i).unwrap_or(&0);
        if next != have {
            return next > have;
        }
    }
    false
}

fn version_parts(value: &str) -> Vec<u64> {
    let value = value.trim().trim_start_matches(['v', 'V']);
    let numeric = value.split(['-', '+']).next().unwrap_or(value);
    numeric
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

pub fn cleanup_previous() {
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::fs::remove_file(sibling(&exe, ".old"));
    let _ = std::fs::remove_file(sibling(&exe, ".update"));
}

pub fn check() -> UpdateReport {
    let url = format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest");
    let client = match http_client() {
        Ok(client) => client,
        Err(err) => return UpdateReport::Failed(err),
    };
    let response = match client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
    {
        Ok(response) => response,
        Err(err) => return UpdateReport::Failed(format!("没有连上 GitHub：{err}")),
    };
    if response.status().as_u16() == 404 {
        return UpdateReport::None;
    }
    if !response.status().is_success() {
        return UpdateReport::Failed(format!("检查更新失败（HTTP {}）。", response.status()));
    }
    let body = match response.text() {
        Ok(body) => body,
        Err(err) => return UpdateReport::Failed(format!("更新信息没有读完：{err}")),
    };
    match parse_release(&body) {
        Ok(info) if is_newer(&info.version, CURRENT) => UpdateReport::Available(info),
        Ok(_) => UpdateReport::Current,
        Err(err) => UpdateReport::Failed(err),
    }
}

pub fn download_and_swap(url: &str, expected: u64) -> Result<(), String> {
    if expected > MAX_BYTES {
        return Err("更新包大得不正常，已取消。".into());
    }
    let exe = std::env::current_exe().map_err(|err| format!("找不到当前程序：{err}"))?;
    let staged = sibling(&exe, ".update");
    download(url, &staged, expected)?;
    swap_and_schedule(&exe, &staged)
}

fn parse_release(body: &str) -> Result<ReleaseInfo, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|err| format!("更新信息无法解析：{err}"))?;
    let version = value
        .get("tag_name")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim()
        .trim_start_matches(['v', 'V'])
        .to_string();
    if version.is_empty() {
        return Err("发布版本没有版本号。".into());
    }
    let notes = value.get("body").and_then(|item| item.as_str()).unwrap_or("").trim();
    let mut notes: String = notes.chars().take(600).collect();
    if notes.chars().count() == 600 {
        notes.push('…');
    }
    let assets = value.get("assets").and_then(|item| item.as_array()).ok_or("发布里没有文件。")?;
    let asset = assets
        .iter()
        .find(|item| item.get("name").and_then(|name| name.as_str()) == Some(ASSET))
        .ok_or("这个版本没有附带 mengdownloader.exe，不能自动更新。")?;
    let asset_url = asset
        .get("browser_download_url")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    if asset_url.is_empty() {
        return Err("没有找到更新文件的下载地址。".into());
    }
    let asset_size = asset.get("size").and_then(|item| item.as_u64()).unwrap_or(0);
    Ok(ReleaseInfo { version, notes, asset_url, asset_size })
}

fn download(url: &str, dest: &Path, expected: u64) -> Result<(), String> {
    let client = http_client()?;
    let response = client.get(url).send().map_err(|err| format!("下载更新失败：{err}"))?;
    if !response.status().is_success() {
        return Err(format!("下载更新失败（HTTP {}）。", response.status()));
    }
    let bytes = response.bytes().map_err(|err| format!("更新没有下完：{err}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("更新包大得不正常，已取消。".into());
    }
    if expected > 0 && bytes.len() as u64 != expected {
        return Err(format!("更新文件不完整：收到 {} 字节，发布记录是 {} 字节。", bytes.len(), expected));
    }
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(dest, &bytes).map_err(|err| format!("写不了更新文件：{err}"))?;
    Ok(())
}

fn swap_and_schedule(current: &Path, staged: &Path) -> Result<(), String> {
    let old = sibling(current, ".old");
    let _ = std::fs::remove_file(&old);
    if let Err(err) = std::fs::rename(current, &old) {
        let _ = std::fs::remove_file(staged);
        return Err(format!("无法换下正在运行的程序：{err}"));
    }
    if let Err(err) = std::fs::rename(staged, current) {
        let _ = std::fs::rename(&old, current);
        return Err(format!("无法放入新程序：{err}"));
    }
    if let Err(err) = schedule_restart(current) {
        return Err(format!("{err} 新版本文件已经换上，请手动重新打开。"));
    }
    Ok(())
}

fn schedule_restart(exe: &Path) -> Result<(), String> {
    let exe_text = exe.display().to_string().replace('\'', "''");
    let pid = std::process::id();
    let script_path = std::env::temp_dir().join(format!("meng-update-{pid}.ps1"));
    let script_text = script_path.display().to_string().replace('\'', "''");
    let script = format!(
        "$id = {pid}\r\nwhile (Get-Process -Id $id -ErrorAction SilentlyContinue) {{ Start-Sleep -Milliseconds 400 }}\r\nStart-Process -FilePath '{exe_text}'\r\nRemove-Item -LiteralPath '{script_text}' -Force\r\n"
    );
    std::fs::write(&script_path, script).map_err(|err| format!("写不了更新脚本：{err}"))?;
    spawn_hidden_powershell(&script_path)
}

fn spawn_hidden_powershell(script: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new("powershell");
    command.args([
        "-NoProfile",
        "-WindowStyle",
        "Hidden",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]);
    command.arg(script);
    spawn_detached(&mut command)
}

fn spawn_detached(command: &mut std::process::Command) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED: u32 = 0x0000_0008;
        const NEW_GROUP: u32 = 0x0000_0200;
        const BREAKAWAY: u32 = 0x0100_0000;
        if command.creation_flags(DETACHED | NEW_GROUP | BREAKAWAY).spawn().is_ok() {
            return Ok(());
        }
        return command
            .creation_flags(DETACHED | NEW_GROUP)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("没有安排重启：{err}"));
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        Err("自动更新只支持 Windows。".into())
    }
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("mengdownloader/{CURRENT}"))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|err| format!("更新用的网络客户端没有建起来：{err}"))
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn release_json_picks_the_exe_asset() {
        let body = r#"{
            "tag_name": "v0.2.0",
            "body": "修复更新",
            "assets": [
                {"name": "mengdownloader-0.2.0-windows-x64.zip", "browser_download_url": "https://example.test/app.zip", "size": 10},
                {"name": "mengdownloader.exe", "browser_download_url": "https://example.test/mengdownloader.exe", "size": 20}
            ]
        }"#;
        let info = super::parse_release(body).unwrap();
        assert_eq!(info.version, "0.2.0");
        assert_eq!(info.asset_url, "https://example.test/mengdownloader.exe");
        assert_eq!(info.asset_size, 20);
        assert!(super::is_newer(&info.version, super::CURRENT));
    }

    #[test]
    fn version_order() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("", "0.1.0"));
    }
}
