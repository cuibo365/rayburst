//! Self-hosted Baidu Netdisk interface (扫码登录 + 文件列表), so the app can
//! render its own drive UI and talk to Baidu's APIs from the Rust backend,
//! without embedding the Baidu web page.
//!
//! Credentials (BDUSS/PTOKEN/STOKEN) are stored in `app_data_dir/baidu_cred.json`
//! after a successful QR-code login and reused for subsequent sessions.

use crate::commands::cloud_drive::{http_client, CloudDriveFile};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const PASSPORT_BASE: &str = "https://passport.baidu.com";
const PAN_REST: &str = "https://pan.baidu.com/rest/2.0/xpan/file";
const CRED_FILE: &str = "baidu_cred.json";

/// Stored Baidu session credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaiduCredential {
    pub bduss: String,
    #[serde(default)]
    pub ptoken: String,
    #[serde(default)]
    pub stoken: String,
}

/// Payload returned for QR-code login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaiduQrcode {
    pub imgurl: String,
    pub sign: String,
}

/// Status of a QR-login poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaiduQrStatus {
    /// ok | waiting | expired | fail
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bduss: Option<String>,
}

fn cred_path(app: &AppHandle) -> Result<PathBuf, AppError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::CloudDrive(format!("无法定位应用数据目录: {e}")))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::CloudDrive(format!("无法创建应用数据目录: {e}")))?;
    Ok(dir.join(CRED_FILE))
}

/// Read the stored BDUSS, if any.
pub fn stored_bduss(app: &AppHandle) -> Option<String> {
    let path = cred_path(app).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let cred: BaiduCredential = serde_json::from_str(&text).ok()?;
    if cred.bduss.is_empty() {
        return None;
    }
    Some(cred.bduss)
}

fn save_cred(app: &AppHandle, cred: &BaiduCredential) -> Result<(), AppError> {
    let path = cred_path(app)?;
    let text = serde_json::to_string_pretty(cred)
        .map_err(|e| AppError::CloudDrive(format!("序列化凭据失败: {e}")))?;
    std::fs::write(&path, text)
        .map_err(|e| AppError::CloudDrive(format!("保存凭据失败: {e}")))
}

fn clear_cred(app: &AppHandle) {
    if let Ok(path) = cred_path(app) {
        let _ = std::fs::remove_file(path);
    }
}

/// Request a QR code for login. Returns the QR image URL and its sign token.
#[tauri::command]
pub async fn cloud_drive_baidu_qrcode(app: AppHandle) -> Result<BaiduQrcode, AppError> {
    let client = http_client()?;
    // `tpl=netdisk` scopes the QR code to the Netdisk product (same as the web app).
    let url = format!(
        "{PASSPORT_BASE}/v2/api/getqrcode?lp=pc&qrloginfrom=pc&apiver=v3&tpl=netdisk&subpro=netdisk_web"
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求百度二维码失败: {e}")))?;
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取二维码响应失败: {e}")))?;
    log::debug!("cloud-drive: getqrcode resp (truncated): {}", &text[..text.len().min(300)]);

    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        status: Option<i64>,
        data: Option<RawData>,
    }
    #[derive(Deserialize)]
    struct RawData {
        #[serde(default)]
        imgurl: Option<String>,
        #[serde(default)]
        sign: Option<String>,
        #[serde(default)]
        code_string: Option<String>,
    }
    let raw: Raw = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析二维码响应失败: {e}")))?;
    let data = raw.data.ok_or_else(|| AppError::CloudDrive("二维码响应缺少 data".into()))?;
    let imgurl = data.imgurl.unwrap_or_default();
    let sign = data
        .sign
        .or(data.code_string)
        .unwrap_or_default();
    if imgurl.is_empty() || sign.is_empty() {
        return Err(AppError::CloudDrive(format!(
            "二维码响应字段缺失 (status={:?}, raw={})",
            raw.status,
            &text[..text.len().min(200)]
        )));
    }
    Ok(BaiduQrcode { imgurl, sign })
}

/// Poll the login state for a QR code. On success the session is stored and
/// `bduss` is returned in the payload.
#[tauri::command]
pub async fn cloud_drive_baidu_qrlogin(
    app: AppHandle,
    sign: String,
) -> Result<BaiduQrStatus, AppError> {
    let client = http_client()?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let url = format!(
        "{PASSPORT_BASE}/v2/api/qrlogin?apiver=v3&sign={sign}&charset=UTF-8&tk=&t={ts}"
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求登录状态失败: {e}")))?;
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取登录状态响应失败: {e}")))?;
    log::debug!("cloud-drive: qrlogin resp (truncated): {}", &text[..text.len().min(300)]);

    // Tolerate several known response shapes:
    //   {"errno":0,"data":{"status":"ok","bduss":"...","ptoken":"...","stoken":"..."}}
    //   {"errno":1}                                        -> not scanned yet
    //   {"errno":0,"data":{"status":"waiting"|"expired"|"fail",...}}
    //   {"status":0|1,"data":{...}}
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        errno: Option<i64>,
        #[serde(default)]
        status: Option<i64>,
        data: Option<RawData>,
    }
    #[derive(Deserialize)]
    struct RawData {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        bduss: Option<String>,
        #[serde(default)]
        ptoken: Option<String>,
        #[serde(default)]
        stoken: Option<String>,
    }

    let raw: Raw = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析登录状态响应失败: {e}")))?;

    if let Some(data) = &raw.data {
        let state = data.status.as_deref().unwrap_or("");
        if state == "ok" {
            if let Some(bduss) = &data.bduss {
                if !bduss.is_empty() {
                    save_cred(
                        &app,
                        &BaiduCredential {
                            bduss: bduss.clone(),
                            ptoken: data.ptoken.clone().unwrap_or_default(),
                            stoken: data.stoken.clone().unwrap_or_default(),
                        },
                    )?;
                    return Ok(BaiduQrStatus {
                        status: "ok".into(),
                        bduss: Some(bduss.clone()),
                    });
                }
            }
            // status ok without bduss is unexpected -> treat as waiting.
            return Ok(BaiduQrStatus { status: "waiting".into(), bduss: None });
        }
        if state == "waiting" || state == "expired" || state == "fail" {
            return Ok(BaiduQrStatus { status: state.into(), bduss: None });
        }
    }

    // Unknown shape (e.g. {"errno":1} before scanning): keep waiting.
    Ok(BaiduQrStatus { status: "waiting".into(), bduss: None })
}

/// Whether a stored session exists.
#[tauri::command]
pub fn cloud_drive_baidu_login_status(app: AppHandle) -> Result<bool, AppError> {
    Ok(stored_bduss(&app).is_some())
}

/// Forget the stored session.
#[tauri::command]
pub fn cloud_drive_baidu_logout(app: AppHandle) -> Result<(), AppError> {
    clear_cred(&app);
    Ok(())
}

/// List the files in a Baidu Netdisk directory (`dir` is a full path, e.g. "/").
#[tauri::command]
pub async fn cloud_drive_baidu_list(app: AppHandle, dir: String) -> Result<Vec<CloudDriveFile>, AppError> {
    let bduss = stored_bduss(&app)
        .ok_or_else(|| AppError::CloudDrive("未登录百度网盘，请先扫码登录".into()))?;
    let client = http_client()?;
    let url = format!(
        "{PAN_REST}?method=list&dir={}&showempty=1&limit=1000&web=1",
        urlencoding::encode(&dir)
    );
    let resp = client
        .get(&url)
        .header("Cookie", format!("BDUSS={bduss}"))
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
        });
    }
    Ok(out)
}
