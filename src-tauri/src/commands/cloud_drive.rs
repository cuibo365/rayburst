//! Cloud drive (网盘) integration.
//!
//! Self-hosted Baidu Netdisk interface: OAuth authorization (QR login + code /
//! access_token paste-back) -> access_token -> xpan list / filemetas dlink ->
//! rayrburst aria2. The refresh_token is permanent, so token renewal is a pure
//! API call and never re-opens the authorization page.
//!
//! Download chain (all public Baidu APIs):
//!   OAuth access_token -> filemetas (dlink) -> rayrburst aria2 (via `downloads::submit`).

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const USER_AGENT: &str = "pan.baidu.com";
/// Registered OAuth app (alistgo's Baidu client) that supports QR login on the
/// authorize page (`qrcode=1`). The callback lands on alistgo.com; the user
/// pastes the resulting code/access_token back into Rayburst.
const OAUTH_CLIENT_ID: &str = "hq9yQ9w9kR4YHj1kyYafLygVocobh7Sf";
const OAUTH_CLIENT_SECRET: &str = "YH2VpZcFJHYNnV6vLfHQXDBhcE7ZChyE";
const OAUTH_REDIRECT_URI: &str = "https://alistgo.com/tool/baidu/callback";
const OAUTH_AUTHORIZE_URL: &str = "https://openapi.baidu.com/oauth/2.0/authorize";
const OAUTH_TOKEN_URL: &str = "https://openapi.baidu.com/oauth/2.0/token";
const BAIDU_FILE_METAS_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/multimedia";
const BAIDU_APP_ID: u32 = 250528;
const PAN_REFERER: &str = "https://pan.baidu.com/";
/// List a folder's children (used to expand checked folders into files).
const BAIDU_FILE_LIST_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/file?method=list&showempty=1";

/// A file checked by the user in the Baidu Netdisk page.
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
    /// Last-modified unix timestamp (seconds), from the xpan list API.
    #[serde(default)]
    pub mtime: u64,
    /// Baidu file category (1 video, 2 audio, 3 image, 4 doc, 5 app, 6 other, 7 torrent).
    #[serde(default)]
    pub category: u32,
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

/// Download checked files: access_token -> dlink -> aria2.
#[tauri::command]
pub async fn cloud_drive_baidu_download(
    app: AppHandle,
    files: Vec<CloudDriveFile>,
    current_dir: Option<String>,
) -> Result<CloudDriveResult, AppError> {
    if files.is_empty() {
        return Err(AppError::CloudDrive("没有选择文件".into()));
    }

    let token = get_access_token(&app).await?;
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

/// Resolve a usable access_token: cached token if still valid, otherwise
/// renew via the permanent refresh_token (pure API, no user interaction).
pub async fn get_access_token(app: &AppHandle) -> Result<String, AppError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Some((token, refresh, expires_at)) = crate::commands::baidu_drive::stored_oauth(app)
    else {
        return Err(AppError::CloudDrive(
            "未授权百度网盘，请先获取并填入 access_token".into(),
        ));
    };
    if expires_at > now {
        log::debug!("cloud-drive: using cached access_token");
        return Ok(token);
    }
    if refresh.is_empty() {
        return Err(AppError::CloudDrive(
            "access_token 已过期且无 refresh_token，请重新获取并填入".into(),
        ));
    }
    refresh_access_token(app, &refresh).await
}

/// Renew access_token using the permanent refresh_token.
async fn refresh_access_token(app: &AppHandle, refresh: &str) -> Result<String, AppError> {
    let client = http_client()?;
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", OAUTH_CLIENT_ID),
            ("client_secret", OAUTH_CLIENT_SECRET),
        ])
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("刷新 access_token 失败: {e}")))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析 token 响应失败: {e}")))?;
    let access = json["access_token"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("token 响应缺少 access_token".into()))?
        .to_string();
    let new_refresh = json["refresh_token"].as_str().unwrap_or("").to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(25 * 24 * 3600);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::commands::baidu_drive::save_oauth_tokens(app, &access, &new_refresh, now + expires_in)?;
    Ok(access)
}

/// Save a user-provided refresh_token: exchange it for a fresh access_token
/// via Baidu's token API and persist both (so future renewals are automatic).
#[tauri::command]
pub async fn cloud_drive_baidu_save_token(
    app: AppHandle,
    token_or_code: String,
) -> Result<String, AppError> {
    let input = token_or_code.trim();
    if input.is_empty() {
        return Err(AppError::CloudDrive("请输入 refresh_token".into()));
    }
    let access = refresh_access_token(&app, input).await?;
    Ok(format!("已用 refresh_token 换取并保存 access_token（{access}）"))
}

/// The OAuth authorize URL (QR login) the user opens to obtain a code/token.
#[tauri::command]
pub fn cloud_drive_baidu_oauth_url() -> Result<String, AppError> {
    Ok(format!(
        "{OAUTH_AUTHORIZE_URL}?response_type=code&client_id={OAUTH_CLIENT_ID}&redirect_uri={OAUTH_REDIRECT_URI}&scope=basic,netdisk&qrcode=1"
    ))
}

/// Whether an authorized session exists (access_token present).
#[tauri::command]
pub fn cloud_drive_baidu_oauth_status(app: AppHandle) -> Result<bool, AppError> {
    Ok(crate::commands::baidu_drive::has_oauth(&app))
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
                mtime: child["server_mtime"].as_u64().unwrap_or(0),
                category: child["category"].as_u64().unwrap_or(0) as u32,
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
                // Baidu 403 防护：dlink 必须附带 access_token 才放行（不带则直接 403）。
                let dlink = if dlink.contains("access_token=") {
                    dlink.to_string()
                } else {
                    format!("{dlink}&access_token={token}")
                };
                result.push((file.clone(), dlink));
            }
        }
    }

    if result.is_empty() {
        return Err(AppError::CloudDrive("未能获取任何文件的下载地址".into()));
    }
    Ok(result)
}
