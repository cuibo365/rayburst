//! Self-hosted Baidu Netdisk interface (OAuth 授权 + 文件列表), so the app can
//! render its own drive UI and talk to Baidu's open APIs from the Rust backend,
//! without embedding the Baidu web page.
//!
//! Credentials (access_token / refresh_token) are stored in
//! `app_data_dir/baidu_cred.json` after a successful OAuth authorization and
//! reused for subsequent sessions. The refresh_token is permanent, so token
//! renewal is a pure API call and never re-opens the authorization page.

use crate::commands::cloud_drive::{get_access_token, http_client, CloudDriveFile};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const PAN_REST: &str = "https://pan.baidu.com/rest/2.0/xpan/file";
const CRED_FILE: &str = "baidu_cred.json";

/// Stored Baidu OAuth credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaiduCredential {
    /// OAuth access_token (valid ~30 days), used for xpan list/filemetas.
    #[serde(default)]
    pub access_token: String,
    /// Permanent refresh_token used to renew access_token without user
    /// interaction.
    #[serde(default)]
    pub refresh_token: String,
    /// Unix seconds when `access_token` expires.
    #[serde(default)]
    pub token_expires_at: u64,
}

pub fn cred_path(app: &AppHandle) -> Result<PathBuf, AppError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::CloudDrive(format!("无法定位应用数据目录: {e}")))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::CloudDrive(format!("无法创建应用数据目录: {e}")))?;
    Ok(dir.join(CRED_FILE))
}

/// Read the stored OAuth credentials, if any.
pub fn stored_oauth(app: &AppHandle) -> Option<(String, String, u64)> {
    let path = cred_path(app).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cred: BaiduCredential = serde_json::from_str(&text).ok()?;
    if cred.access_token.is_empty() {
        return None;
    }
    Some((cred.access_token, cred.refresh_token, cred.token_expires_at))
}

/// Persist (or update) the OAuth credentials.
pub fn save_oauth_tokens(
    app: &AppHandle,
    access_token: &str,
    refresh_token: &str,
    expires_at: u64,
) -> Result<(), AppError> {
    let path = cred_path(app)?;
    let mut cred: BaiduCredential = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(BaiduCredential {
            access_token: String::new(),
            refresh_token: String::new(),
            token_expires_at: 0,
        });
    cred.access_token = access_token.to_string();
    if !refresh_token.is_empty() {
        cred.refresh_token = refresh_token.to_string();
    }
    cred.token_expires_at = expires_at;
    let text = serde_json::to_string_pretty(&cred)
        .map_err(|e| AppError::CloudDrive(format!("序列化凭据失败: {e}")))?;
    std::fs::write(&path, text)
        .map_err(|e| AppError::CloudDrive(format!("保存凭据失败: {e}")))
}

/// Delete the credential file (logout).
pub fn clear_cred(app: &AppHandle) {
    if let Ok(path) = cred_path(app) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether a usable OAuth session exists (access_token present).
pub fn has_oauth(app: &AppHandle) -> bool {
    stored_oauth(app).is_some()
}

#[tauri::command]
pub fn cloud_drive_baidu_login_status(app: AppHandle) -> Result<bool, AppError> {
    Ok(has_oauth(&app))
}

#[tauri::command]
pub fn cloud_drive_baidu_logout(app: AppHandle) -> Result<(), AppError> {
    clear_cred(&app);
    Ok(())
}

/// List a folder's children via Baidu's open API (xpan/file + access_token).
#[tauri::command]
pub async fn cloud_drive_baidu_list(
    app: AppHandle,
    dir: String,
) -> Result<Vec<CloudDriveFile>, AppError> {
    let token = get_access_token(&app).await?;
    let client = http_client()?;
    let url = format!(
        "{PAN_REST}?method=list&dir={}&showempty=1&limit=1000&web=1&access_token={}",
        urlencoding::encode(&dir),
        token
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求网盘文件列表失败: {e}")))?;
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取文件列表响应失败: {e}")))?;

    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        errno: Option<i64>,
        #[serde(default)]
        list: Option<Vec<RawFile>>,
    }
    #[derive(Deserialize)]
    struct RawFile {
        fs_id: u64,
        #[serde(rename = "server_filename", default)]
        name: String,
        #[serde(default)]
        size: u64,
        #[serde(default)]
        path: String,
        #[serde(default)]
        isdir: u8,
        #[serde(rename = "server_mtime", default)]
        mtime: u64,
        #[serde(default)]
        category: u32,
    }

    let raw: Raw = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析文件列表失败: {e}")))?;
    if raw.errno.unwrap_or(0) != 0 {
        return Err(AppError::CloudDrive(format!(
            "百度返回错误 errno={}",
            raw.errno.unwrap_or(-1)
        )));
    }
    let mut out = Vec::new();
    for f in raw.list.unwrap_or_default() {
        out.push(CloudDriveFile {
            fs_id: f.fs_id,
            name: f.name,
            size: f.size,
            path: f.path,
            isdir: f.isdir != 0,
            mtime: f.mtime,
            category: f.category,
        });
    }
    Ok(out)
}
