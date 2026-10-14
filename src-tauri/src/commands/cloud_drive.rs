//! Cloud drive (网盘) integration.
//!
//! The Baidu Netdisk web version runs in a child webview that fills the app's
//! content area (see `cloud_drive_baidu_open`). An initialization script
//! (`resources/cloud-drive-inject.js`) is injected into that webview so the
//! page can mount a "下载到 Rayburst" button and call the download command.
//!
//! Download chain (all public Baidu APIs, same as LinkSwift):
//!   WebView cookie store -> BDUSS -> openapi authorize -> access_token
//!   -> filemetas (dlink) -> rayrburst aria2 (via `downloads::submit`).

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
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

/// Injection script embedded at compile time.
const INJECT_SCRIPT: &str = include_str!("../../resources/cloud-drive-inject.js");

/// Logical bounds for the embedded netdisk window (relative to the main window).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WebviewBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Last reported content-area bounds (relative to the main window). Kept so the
/// overlay window can be re-aligned when the main window moves or resizes.
static CLOUD_BOUNDS: Mutex<Option<WebviewBounds>> = Mutex::new(None);

/// Convert main-window-relative logical bounds to screen logical coordinates by
/// adding the main window's inner (client-area) position, accounting for DPI.
fn screen_geometry_from_bounds(
    app: &AppHandle,
    bounds: WebviewBounds,
) -> Option<(tauri::LogicalPosition<f64>, tauri::LogicalSize<f64>)> {
    let main = app.get_window("main")?;
    let inner = main.inner_position().ok()?;
    let scale = main.scale_factor().ok()?;
    let x = inner.x as f64 / scale + bounds.x;
    let y = inner.y as f64 / scale + bounds.y;
    Some((
        tauri::LogicalPosition::new(x, y),
        tauri::LogicalSize::new(bounds.width, bounds.height),
    ))
}

/// Register main-window move/resize listeners so the netdisk overlay window
/// follows the main window.
pub fn init_main_window_sync(app: &tauri::AppHandle) {
    let Some(main) = app.get_window("main") else {
        return;
    };
    let handle = app.clone();
    main.on_window_event(move |event| {
        let win = handle.get_webview_window(WEBVIEW_LABEL);
        let Some(win) = win else {
            return;
        };
        let bounds = *CLOUD_BOUNDS.lock().unwrap();
        match event {
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                if let Some(bounds) = bounds {
                    if let Some((pos, size)) = screen_geometry_from_bounds(&handle, bounds) {
                        let _ = win.set_position(pos);
                        let _ = win.set_size(size);
                    }
                }
            }
            _ => {}
        }
    });
}

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

fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
}

fn pan_url() -> Result<tauri::Url, AppError> {
    PAN_URL
        .parse()
        .map_err(|e| AppError::CloudDrive(format!("URL 解析失败: {e}")))
}

/// Create (or show) the Baidu Netdisk window overlaying the content area.
///
/// Implemented as a standalone borderless `WebviewWindow` (stable API) rather
/// than a child webview: child webviews are unstable on Windows and can fail
/// with WebView2 error 0x8007139F while freezing the main thread.
#[tauri::command]
pub async fn cloud_drive_baidu_open(
    app: AppHandle,
    bounds: WebviewBounds,
) -> Result<(), AppError> {
    *CLOUD_BOUNDS.lock().unwrap() = Some(bounds);
    spawn_url_probe(app.clone());
    let (pos, size) = screen_geometry_from_bounds(&app, bounds)
        .unwrap_or((tauri::LogicalPosition::new(bounds.x, bounds.y), tauri::LogicalSize::new(bounds.width, bounds.height)));

    if let Some(win) = app.get_webview_window(WEBVIEW_LABEL) {
        let _ = win.set_position(pos);
        let _ = win.set_size(size);
        let _ = win.show();
        let _ = win.set_focus();
        match win.url() {
            Ok(url) => {
                log::info!("cloud-drive: reopening window at {}", url);
                if url.scheme() == "about" || url.as_str() == "about:blank" {
                    let _ = win.reload();
                }
                return Ok(());
            }
            Err(e) => {
                // Renderer is unresponsive (IPC to the webview failed). Reloading
                // a wedged WebView2 process is unreliable, so destroy the window
                // and recreate it below for a clean WebView2 process.
                log::warn!("cloud-drive: window unresponsive ({e}), destroying and recreating");
                let _ = win.destroy();
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }        }
    }

    // Standard Edge/Chrome UA so Baidu serves the full page (WebView2's default
    // UA can trigger a compatibility downgrade / blank render on some pan pages).
    let win = tauri::WebviewWindowBuilder::new(&app, WEBVIEW_LABEL, tauri::WebviewUrl::External(pan_url()?))
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0",
        )
        .initialization_script(INJECT_SCRIPT)
        .position(pos.x, pos.y)
        .inner_size(size.width, size.height)
        .decorations(false)
        .skip_taskbar(true)
        .shadow(false)
        .resizable(false)
        .build()
        .map_err(|e| AppError::CloudDrive(format!("创建网盘窗口失败: {e}")))?;
    let _ = win.set_focus();
    if let Ok(url) = win.url() {
        log::info!("cloud-drive: created window, url={url}");
    }
    Ok(())
}

/// Log the overlay window's URL every 2s for ~10s after open, so a blank page
/// can be traced to which URL the webview actually landed on (pan home page,
/// passport login redirect, or nothing at all).
fn spawn_url_probe(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        for i in 0..5 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            match app.get_webview_window(WEBVIEW_LABEL) {
                Some(win) => match win.url() {
                    Ok(url) => log::info!("cloud-drive: url probe #{i} -> {url}"),
                    Err(e) => log::warn!("cloud-drive: url probe #{i} unavailable ({e})"),
                },
                None => {
                    log::warn!("cloud-drive: url probe #{i} window gone");
                    return;
                }
            }
        }
    });
}

/// Update the overlay window's bounds (called on window/content resize).
#[tauri::command]
pub async fn cloud_drive_baidu_resize(
    app: AppHandle,
    bounds: WebviewBounds,
) -> Result<(), AppError> {
    *CLOUD_BOUNDS.lock().unwrap() = Some(bounds);
    if let Some(win) = app.get_webview_window(WEBVIEW_LABEL) {
        if let Some((pos, size)) = screen_geometry_from_bounds(&app, bounds) {
            let _ = win.set_position(pos);
            let _ = win.set_size(size);
        }
    }
    Ok(())
}

/// Hide the overlay window and tell the frontend to restore its own view.
#[tauri::command]
pub async fn cloud_drive_baidu_close(app: AppHandle) -> Result<(), AppError> {
    if let Some(win) = app.get_webview_window(WEBVIEW_LABEL) {
        let _ = win.hide();
    }
    let _ = app.emit("cloud-drive:hidden", ());
    Ok(())
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

/// Read BDUSS from the (shared) WebView cookie store.
async fn read_bduss(app: &AppHandle) -> Result<String, AppError> {
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
        "未检测到百度网盘登录状态，请先在网盘页面完成登录".into(),
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
