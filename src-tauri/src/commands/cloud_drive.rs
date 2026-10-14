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
use tauri::{AppHandle, Emitter, Manager};

const PAN_URL: &str = "https://pan.baidu.com/disk/main";
const WEBVIEW_LABEL: &str = "cloud-drive-baidu";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
/// Public client_id used by the Baidu Netdisk web app (same as LinkSwift).
const BAIDU_CLIENT_ID: &str = "omiOnr2tYnN9vSyDErcVFWpPU2mZA7YO";
const BAIDU_AUTHORIZE_URL: &str = "https://openapi.baidu.com/oauth/2.0/authorize";
const BAIDU_FILE_METAS_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/multimedia";
const BAIDU_APP_ID: u32 = 250528;
const PAN_REFERER: &str = "https://pan.baidu.com/";

/// Injection script embedded at compile time.
const INJECT_SCRIPT: &str = include_str!("../../resources/cloud-drive-inject.js");

/// Logical bounds for the embedded netdisk webview (relative to the window).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WebviewBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A file checked by the user in the Baidu Netdisk page (sent by the injected script).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudDriveFile {
    pub fs_id: u64,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub path: String,
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

/// Create (or show) the embedded Baidu Netdisk webview filling the content area.
#[tauri::command]
pub async fn cloud_drive_baidu_open(
    app: AppHandle,
    bounds: WebviewBounds,
) -> Result<(), AppError> {
    if let Some(webview) = app.get_webview(WEBVIEW_LABEL) {
        let _ = webview.set_position(tauri::LogicalPosition::new(bounds.x, bounds.y));
        let _ = webview.set_size(tauri::LogicalSize::new(bounds.width, bounds.height));
        let _ = webview.show();
        let _ = webview.set_focus();
        return Ok(());
    }

    let window = app
        .get_window("main")
        .ok_or_else(|| AppError::CloudDrive("主窗口不存在".into()))?;
    let builder = tauri::webview::WebviewBuilder::new(WEBVIEW_LABEL, tauri::WebviewUrl::External(pan_url()?))
        .initialization_script(INJECT_SCRIPT);
    let webview = window
        .add_child(
            builder,
            tauri::LogicalPosition::new(bounds.x, bounds.y),
            tauri::LogicalSize::new(bounds.width, bounds.height),
        )
        .map_err(|e| AppError::CloudDrive(format!("创建网盘窗口失败: {e}")))?;
    let _ = webview.set_focus();
    Ok(())
}

/// Update the embedded webview's bounds (called on window/content resize).
#[tauri::command]
pub async fn cloud_drive_baidu_resize(
    app: AppHandle,
    bounds: WebviewBounds,
) -> Result<(), AppError> {
    if let Some(webview) = app.get_webview(WEBVIEW_LABEL) {
        let _ = webview.set_position(tauri::LogicalPosition::new(bounds.x, bounds.y));
        let _ = webview.set_size(tauri::LogicalSize::new(bounds.width, bounds.height));
    }
    Ok(())
}

/// Hide the embedded webview and tell the frontend to restore its own view.
#[tauri::command]
pub async fn cloud_drive_baidu_close(app: AppHandle) -> Result<(), AppError> {
    if let Some(webview) = app.get_webview(WEBVIEW_LABEL) {
        let _ = webview.hide();
    }
    let _ = app.emit("cloud-drive:hidden", ());
    Ok(())
}

/// Download checked files: cookie -> token -> dlink -> aria2.
#[tauri::command]
pub async fn cloud_drive_baidu_download(
    app: AppHandle,
    files: Vec<CloudDriveFile>,
) -> Result<CloudDriveResult, AppError> {
    if files.is_empty() {
        return Err(AppError::CloudDrive("没有选择文件".into()));
    }

    let bduss = read_bduss(&app).await?;
    let token = exchange_access_token(&bduss).await?;
    let links = fetch_dlinks(&token, &files).await?;

    let mut added = 0usize;
    let mut failed = 0usize;
    for (file, dlink) in &links {
        let options = serde_json::json!({
            "header": [
                format!("User-Agent: {USER_AGENT}"),
                format!("Referer: {PAN_REFERER}"),
            ],
            "out": file.name,
        });
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
