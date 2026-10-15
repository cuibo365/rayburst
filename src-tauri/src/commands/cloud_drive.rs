//! Cloud drive (网盘) integration.
//!
//! The Baidu Netdisk web version runs **inside the main window's webview** in
//! full-screen navigation mode (see `cloud_drive_baidu_nav_open`). A bridge
//! script (`resources/cloud-drive-inject.js`) is injected so the page can
//! mount a "返回 Rayburst" button and a download button calling the download
//! command.
//!
//! Download chain (all public Baidu APIs, same as LinkSwift):
//!   WebView cookie store -> BDUSS -> openapi authorize -> access_token
//!   -> filemetas (dlink) -> rayrburst aria2 (via `downloads::submit`).

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_store::StoreExt;

const PAN_URL: &str = "https://pan.baidu.com/disk/main";
const WEBVIEW_LABEL: &str = "cloud-drive-baidu";
const USER_AGENT: &str = "pan.baidu.com";
/// Public client_id used by the Baidu Netdisk web app (same as LinkSwift).
const BAIDU_CLIENT_ID: &str = "omiOnr2tYnN9vSyDErcVFWpPU2mZA7YO";
const BAIDU_AUTHORIZE_URL: &str = "https://openapi.baidu.com/oauth/2.0/authorize";
const BAIDU_FILE_METAS_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/multimedia";
const BAIDU_APP_ID: u32 = 250528;
const PAN_REFERER: &str = "https://pan.baidu.com/";
/// List a folder's children (used to expand checked folders into files).
const BAIDU_FILE_LIST_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/file?method=list&showempty=1";

/// Injection script embedded at compile time (download + back button on the
/// Baidu page). Injected into the main window both via the config-level
/// initializationScript and via repeated eval in spawn_nav_injector.
const INJECT_SCRIPT: &str = include_str!("../../resources/cloud-drive-inject.js");

/// A file checked by the user in the Baidu Netdisk page (sent by the injected script).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudDriveFile {
    pub fs_id: u64,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub path: String,
    /// True for folders; the backend expands them into their contained files.
    #[serde(default)]
    pub isdir: bool,
}

#[derive(Debug, Serialize)]
pub struct CloudDriveResult {
    pub message: String,
    pub added: usize,
    pub failed: usize,
}

pub(crate) fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
}

/// Full-screen navigation mode: navigate the MAIN window's webview (the same
/// one that renders the app UI) to the Baidu Netdisk page, then keep
/// re-injecting the nav control script until the page is ready. Reuses the
/// main webview, so no second WebView2 instance is created (creating a second
/// instance fails/hangs the main thread on some machines).
#[tauri::command]
pub fn cloud_drive_baidu_nav_open(
    app: AppHandle,
    url: Option<String>,
) -> Result<(), AppError> {
    let target = url.unwrap_or_else(|| PAN_URL.to_string());
    let main = app
        .get_webview("main")
        .ok_or_else(|| AppError::CloudDrive("主窗口不存在".into()))?;
    let parsed = target
        .parse::<tauri::Url>()
        .map_err(|e| AppError::CloudDrive(format!("URL 解析失败: {e}")))?;
    log::info!("cloud-drive: navigating main webview to {target}");
    main.navigate(parsed)
        .map_err(|e| AppError::CloudDrive(format!("导航失败: {e}")))?;
    spawn_nav_injector(app.clone());
    Ok(())
}

/// Navigate the main webview back to the app (tauri://localhost). Called by
/// the injected "返回 Rayburst" button on the Baidu page.
#[tauri::command]
pub fn cloud_drive_nav_back(app: AppHandle) -> Result<(), AppError> {
    let main = app
        .get_webview("main")
        .ok_or_else(|| AppError::CloudDrive("主窗口不存在".into()))?;
    let back = "tauri://localhost"
        .parse::<tauri::Url>()
        .map_err(|e| AppError::CloudDrive(format!("URL 解析失败: {e}")))?;
    log::info!("cloud-drive: navigating main webview back to app");
    main.navigate(back)
        .map_err(|e| AppError::CloudDrive(format!("返回失败: {e}")))?;
    Ok(())
}

/// Re-inject the bridge script (download + back button) into the main window
/// until it sticks, in case the page is still loading or the config-level
/// initializationScript was not applied.
fn spawn_nav_injector(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Persistent injection while on the Baidu page: a fixed 20s window can
        // miss slow loads / long login flows, leaving the page with no buttons.
        // Runs every 800ms until the webview navigates back to the app *after*
        // having been on the Baidu page (navigation is async, so right after
        // nav_open the URL may still be tauri://localhost — do not exit then).
        let mut was_on_pan = false;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            let Some(main) = app.get_webview("main") else {
                return;
            };
            match main.url() {
                Ok(url) => {
                    if url.scheme() == "tauri" {
                        if was_on_pan {
                            log::debug!("cloud-drive: injector stopped (back on app page)");
                            return;
                        }
                        continue; // navigation in flight; keep waiting
                    }
                    if url.host_str() == Some("pan.baidu.com") && url.path().starts_with("/disk/") {
                        was_on_pan = true;
                        let _ = main.eval(INJECT_SCRIPT);
                    }
                }
                Err(e) => {
                    // Webview is busy mid-navigation; retry next tick.
                    log::debug!("cloud-drive: injector url unavailable ({e})");
                }
            }
        }
    });
}

/// Download checked files: cookie -> token -> dlink -> aria2.
#[tauri::command]
pub async fn cloud_drive_baidu_download(
    app: AppHandle,
    files: Vec<CloudDriveFile>,
    current_dir: Option<String>,
) -> Result<CloudDriveResult, AppError> {
    if files.is_empty() {
        return Err(AppError::CloudDrive("没有选择文件".into()));
    }

    let bduss = read_bduss(&app).await?;
    let token = exchange_access_token(&bduss).await?;
    // Expand checked folders into their contained files (recursively), so both
    // files and folders can be downloaded like LinkSwift.
    let files = expand_folders(&token, files).await?;
    if files.is_empty() {
        return Err(AppError::CloudDrive("所选文件夹中没有可下载的文件".into()));
    }
    let links = fetch_dlinks(&token, &files).await?;

    // Rayburst's configured default download directory (config.json -> preferences.dir).
    let default_dir: Option<String> = app
        .store("config.json")
        .ok()
        .and_then(|store| store.get("preferences"))
        .and_then(|prefs| {
            prefs
                .get("dir")?
                .as_str()
                .map(str::trim)
                .map(str::to_string)
        })
        .filter(|dir| !dir.is_empty());

    let mut added = 0usize;
    let mut failed = 0usize;
    for (file, dlink) in &links {
        let mut options = serde_json::json!({
            "header": [
                format!("User-Agent: {USER_AGENT}"),
                format!("Referer: {PAN_REFERER}"),
            ],
            "out": file.name,
        });
        // dir = default download dir + (file path minus the current browser folder path).
        if let Some(dir) = default_dir
            .as_deref()
            .and_then(|base| resolve_download_dir(base, current_dir.as_deref(), &file.path))
        {
            options["dir"] = serde_json::Value::String(dir);
        }
        match crate::services::downloads::submit(
            &app,
            crate::services::downloads::TaskInput::Uris(vec![dlink.clone()]),
            options,
            None,
        )
        .await
        {
            Ok(_) => added += 1,
            Err(e) => {
                failed += 1;
                log::warn!("cloud-drive: aria2 submit failed for {}: {e}", file.name);
            }
        }
    }

    let message = if failed == 0 {
        format!("已添加 {added} 个文件到下载队列")
    } else {
        format!("已添加 {added} 个文件，{failed} 个失败")
    };
    Ok(CloudDriveResult {
        message,
        added,
        failed,
    })
}

/// Compute the aria2 `dir` for a Baidu file:
///   default download dir + (file path minus the current browser folder path)
/// Returns `None` when there is no default dir, no current dir, or the file
/// path does not live under the current folder (fall back to engine default).
fn resolve_download_dir(
    default_dir: &str,
    current_dir: Option<&str>,
    file_path: &str,
) -> Option<String> {
    let base = default_dir.trim();
    if base.is_empty() {
        return None;
    }
    let cur = current_dir.map(str::trim).filter(|s| !s.is_empty())?;
    let fp = file_path.trim_start_matches('/');
    let rest = fp.strip_prefix(cur.trim_end_matches('/').trim_start_matches('/'))?;
    let rel = rest.trim_start_matches('/');
    let joined = if rel.is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches(['/', '\\']), rel)
    };
    Some(sanitize_dir_path(&joined))
}

/// Sanitize a joined download path:
/// - collapse `//` -> `/` and `\\` -> `\`
/// - normalize `\` -> `/` (aria2 accepts `D:/...` on Windows)
/// - drop Windows-illegal chars `< > " | ? *` (and stray `:`), control chars,
///   while keeping the drive-letter colon (`D:/`)
/// - drop `.` / `..` segments so the result cannot escape the base dir
fn sanitize_dir_path(raw: &str) -> String {
    let mut s = raw.replace("\\\\", "\\");
    while s.contains("\\\\") {
        s = s.replace("\\\\", "\\");
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    s = s.replace('\\', "/");
    while s.contains("//") {
        s = s.replace("//", "/");
    }

    let keep_drive = (s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic())
        .then(|| s[..2].to_string());
    let body = if let Some(drive) = &keep_drive {
        &s[2..]
    } else {
        s.as_str()
    };
    let cleaned: String = body
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | ':') && !c.is_control())
        .collect();

    let segments: Vec<&str> = cleaned
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect();

    let mut out = String::new();
    if let Some(drive) = &keep_drive {
        out.push_str(drive);
        out.push('/');
    }
    out.push_str(&segments.join("/"));
    out
}

/// Read BDUSS: prefer the stored session from QR-code login (self-hosted UI),
/// fall back to the (shared) WebView cookie store (in-page web version login).
async fn read_bduss(app: &AppHandle) -> Result<String, AppError> {
    if let Some(bduss) = crate::commands::baidu_drive::stored_bduss(app) {
        log::debug!("cloud-drive: using stored BDUSS session");
        return Ok(bduss);
    }
    let main = app
        .get_webview("main")
        .ok_or_else(|| AppError::CloudDrive("主窗口不可用".into()))?;
    let url: tauri::Url = "https://pan.baidu.com"
        .parse()
        .map_err(|e| AppError::CloudDrive(format!("URL 解析失败: {e}")))?;
    let cookies = main
        .cookies_for_url(url)
        .map_err(|e| AppError::CloudDrive(format!("读取网盘 Cookie 失败: {e}")))?;
    for cookie in cookies {
        if cookie.name() == "BDUSS" {
            return Ok(cookie.value().to_string());
        }
    }
    Err(AppError::CloudDrive(
        "未检测到百度网盘登录状态，请在网盘页面完成登录或扫码登录".into(),
    ))
}

/// Exchange BDUSS for an OAuth access_token using Baidu's public authorize
/// endpoint (same flow as LinkSwift, including the one-click grant POST).
async fn exchange_access_token(bduss: &str) -> Result<String, AppError> {
    let client = http_client()?;
    let url = format!(
        "{BAIDU_AUTHORIZE_URL}?response_type=token&scope=basic,netdisk&client_id={BAIDU_CLIENT_ID}&redirect_uri=oob&confirm_login=0"
    );

    // First request: follow redirects, inspect the final body.
    let authorize = client
        .get(&url)
        .header("Cookie", format!("BDUSS={bduss}"))
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求百度授权失败: {e}")))?;
    let authorize = authorize
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取授权响应失败: {e}")))?;

    if let Some(token) = extract_access_token(&authorize) {
        return Ok(token);
    }

    // Not yet granted: auto-confirm on the authorization page (LinkSwift flow).
    if authorize.contains("authorize") {
        let bdstoken = extract_html_field(&authorize, "bdstoken").ok_or_else(|| {
            AppError::CloudDrive("授权页面缺少 bdstoken，请在浏览器中登录百度网盘后重试".into())
        })?;
        let client_id = extract_html_field(&authorize, "client_id")
            .ok_or_else(|| AppError::CloudDrive("授权页面缺少 client_id".into()))?;
        let form = format!(
            "grant_permissions_arr={}&bdstoken={}&client_id={}&response_type={}&display={}&grant_permissions={}",
            urlencoding::encode("netdisk"),
            urlencoding::encode(&bdstoken),
            urlencoding::encode(&client_id),
            urlencoding::encode("token"),
            urlencoding::encode("page"),
            urlencoding::encode("basic,netdisk"),
        );
        let _ = client
            .post(&url)
            .header("Cookie", format!("BDUSS={bduss}"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("提交授权确认失败: {e}")))?;

        let confirm = client
            .get(&url)
            .header("Cookie", format!("BDUSS={bduss}"))
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("重新请求授权失败: {e}")))?;
        let confirm = confirm
            .text()
            .await
            .map_err(|e| AppError::CloudDrive(format!("读取授权响应失败: {e}")))?;
        if let Some(token) = extract_access_token(&confirm) {
            return Ok(token);
        }
    }

    Err(AppError::CloudDrive(
        "无法获取百度网盘访问令牌，请刷新网盘页面后重试".into(),
    ))
}

fn extract_access_token(body: &str) -> Option<String> {
    let needle = "access_token=";
    let start = body.find(needle)?;
    let rest = &body[start + needle.len()..];
    let end = rest
        .find(|c: char| c == '&' || c == '"' || c == '\'' || c == '<' || c == '>')
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

/// Extract a hidden form field value like `name="bdstoken" value="..."`.
fn extract_html_field(html: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\"");
    let start = html.find(&needle)?;
    let tail = &html[start..];
    let value_start = tail.find("value=\"")? + "value=\"".len();
    let value_end = tail[value_start..].find('"')? + value_start;
    Some(tail[value_start..value_end].to_string())
}

/// Expand checked folders into their contained files (recursively, mirroring
/// LinkSwift's getFilesList). The list API returns full `path` values, so the
/// relative-path -> `dir` mapping keeps working for files inside folders.
/// A folder that fails to list is skipped (warned) so the rest can proceed.
async fn expand_folders(
    token: &str,
    files: Vec<CloudDriveFile>,
) -> Result<Vec<CloudDriveFile>, AppError> {
    let client = http_client()?;
    let mut result: Vec<CloudDriveFile> = Vec::new();
    let mut queue: Vec<CloudDriveFile> = files;
    let mut requests = 0usize;

    while let Some(item) = queue.pop() {
        if !item.isdir {
            result.push(item);
            continue;
        }
        let url = format!(
            "{BAIDU_FILE_LIST_URL}&dir={}&access_token={}",
            urlencoding::encode(&item.path),
            token
        );
        requests += 1;
        // Pace requests like LinkSwift: rest every 50 list calls.
        if requests % 50 == 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let resp = client
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("展开文件夹失败: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析文件夹列表失败: {e}")))?;

        let errno = json["errno"].as_i64().unwrap_or(-1);
        if errno == 9019 {
            return Err(AppError::CloudDrive(
                "百度网盘访问令牌已过期，请刷新网盘页面后重试（errno=9019）".into(),
            ));
        }
        if errno == 112 {
            return Err(AppError::CloudDrive("百度网盘页面已过期，请刷新后重试".into()));
        }
        if errno != 0 {
            log::warn!(
                "cloud-drive: expand folder {} failed (errno={errno})",
                item.path
            );
            continue;
        }
        let Some(list) = json["list"].as_array() else {
            continue;
        };
        for child in list {
            let path = child["path"].as_str().unwrap_or("").to_string();
            if path.is_empty() {
                continue;
            }
            queue.push(CloudDriveFile {
                fs_id: child["fs_id"].as_u64().unwrap_or(0),
                name: child["server_filename"].as_str().unwrap_or("").to_string(),
                size: child["size"].as_u64().unwrap_or(0),
                path,
                isdir: child["isdir"].as_i64().unwrap_or(0) == 1,
            });
        }
    }
    Ok(result)
}

/// Fetch dlink(s) for the selected files (batched by 50, like LinkSwift).
async fn fetch_dlinks(
    token: &str,
    files: &[CloudDriveFile],
) -> Result<Vec<(CloudDriveFile, String)>, AppError> {
    let client = http_client()?;
    let mut result = Vec::new();

    for chunk in files.chunks(50) {
        let fs_ids: Vec<u64> = chunk.iter().map(|f| f.fs_id).collect();
        let fsids_json = serde_json::to_string(&fs_ids)
            .map_err(|e| AppError::CloudDrive(format!("序列化文件 ID 失败: {e}")))?;
        let url = format!(
            "{BAIDU_FILE_METAS_URL}?method=filemetas&dlink=1&fsids={}&access_token={}",
            urlencoding::encode(&fsids_json),
            token
        );
        let resp = client
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("请求下载地址失败: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析下载地址失败: {e}")))?;

        let errno = json["errno"].as_i64().unwrap_or(-1);
        if errno == 9019 {
            return Err(AppError::CloudDrive(
                "百度网盘访问令牌已过期，请刷新网盘页面后重试（errno=9019）".into(),
            ));
        }
        if errno == 112 {
            return Err(AppError::CloudDrive("百度网盘页面已过期，请刷新后重试".into()));
        }
        if errno != 0 {
            let msg = json["errmsg"].as_str().unwrap_or("未知错误");
            return Err(AppError::CloudDrive(format!(
                "获取下载地址失败（errno={errno}）：{msg}"
            )));
        }

        let Some(list) = json["list"].as_array() else {
            return Err(AppError::CloudDrive("下载地址响应缺少文件列表".into()));
        };
        for item in list {
            let Some(fs_id) = item["fs_id"].as_u64() else { continue };
            let Some(dlink) = item["dlink"].as_str() else { continue };
            if let Some(file) = files.iter().find(|f| f.fs_id == fs_id) {
                result.push((file.clone(), dlink.to_string()));
            }
        }
    }

    if result.is_empty() {
        return Err(AppError::CloudDrive("未能获取任何文件的下载地址".into()));
    }
    Ok(result)
}
