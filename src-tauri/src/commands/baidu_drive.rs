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
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

const PASSPORT_BASE: &str = "https://passport.baidu.com";
const PAN_REST: &str = "https://pan.baidu.com/rest/2.0/xpan/file";
const CRED_FILE: &str = "baidu_cred.json";
/// Passport (login) requests must carry a real browser UA; the app-wide
/// `pan.baidu.com` UA is for the aria2 download chain only and is rejected by
/// Baidu's login endpoints.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

fn passport_client(timeout_secs: u64) -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
}

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

/// Session state captured when the QR code was requested: the gid/callback used
/// in the unicast poll and the cookies Set-Cookie'ed by passport at that time.
#[derive(Debug, Clone)]
struct QrSession {
    gid: String,
    callback: String,
    cookie: String,
}

static QR_SESSION: Mutex<Option<QrSession>> = Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Collect `Set-Cookie` header values into a single `name=value; name=value` string.
fn collect_cookies(resp: &reqwest::Response) -> String {
    let mut parts: Vec<String> = Vec::new();
    for v in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = v.to_str() {
            if let Some((name, rest)) = s.split_once('=') {
                let value = rest.split(';').next().unwrap_or("");
                parts.push(format!("{name}={value}"));
            }
        }
    }
    parts.join("; ")
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
pub async fn cloud_drive_baidu_qrcode(_app: AppHandle) -> Result<BaiduQrcode, AppError> {
    let client = passport_client(30)?;
    // Same request shape as the web app: gid/callback/tt/_ are browser-generated
    // values; `tpl=netdisk` scopes the QR code to the Netdisk product.
    let gid = uuid::Uuid::new_v4().to_string().to_uppercase();
    let ts = now_ms();
    let callback = format!("tangram_guid_{ts}");
    let url = format!(
        "{PASSPORT_BASE}/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid={gid}&callback={callback}&apiver=v3&tt={ts}&tpl=netdisk&_={ts}"
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求百度二维码失败: {e}")))?;
    let cookie = collect_cookies(&resp);
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取二维码响应失败: {e}")))?;
    log::info!("cloud-drive: getqrcode resp (truncated): {}", &text[..text.len().min(400)]);

    // Flat shape: {"imgurl":"passport.baidu.com/v2/api/qrcode?sign=...","errno":0,"sign":"..."}
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        errno: Option<i64>,
        #[serde(default)]
        imgurl: Option<String>,
        #[serde(default)]
        sign: Option<String>,
        #[serde(default)]
        code_string: Option<String>,
    }
    let raw: Raw = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析二维码响应失败: {e}")))?;
    if raw.errno.unwrap_or(-1) != 0 {
        return Err(AppError::CloudDrive(format!(
            "百度返回 errno={}（raw={}）",
            raw.errno.unwrap_or(-1),
            &text[..text.len().min(200)]
        )));
    }
    let mut imgurl = raw.imgurl.unwrap_or_default();
    let sign = raw.sign.or(raw.code_string).unwrap_or_default();
    if imgurl.is_empty() || sign.is_empty() {
        return Err(AppError::CloudDrive(format!(
            "二维码响应字段缺失（raw={}）",
            &text[..text.len().min(200)]
        )));
    }
    // The response omits the scheme; the browser resolves it against https://passport.baidu.com.
    if !imgurl.starts_with("http://") && !imgurl.starts_with("https://") {
        imgurl = format!("https://{imgurl}");
    }
    *QR_SESSION.lock().unwrap() = Some(QrSession { gid, callback, cookie });
    Ok(BaiduQrcode { imgurl, sign })
}

/// Exchange the unicast `v` (session ticket) for a BDUSS cookie via qrbdusslogin.
async fn qrbdusslogin(
    client: &reqwest::Client,
    session: &QrSession,
    ticket: &str,
) -> Result<String, AppError> {
    let ts = now_ms();
    let url = format!(
        "{PASSPORT_BASE}/v3/login/main/qrbdusslogin?v={ts}&u={}&bduss={ticket}&loginVersion=v4&qrcode=1&tpl=netdisk&apiver=v3&tt={ts}&traceid=&time={}&alg=v3&sig=rayburst&elapsed=3&shaOne=rayburst&callback=bd__cbs__rayburst",
        urlencoding::encode("https://pan.baidu.com/disk/main"),
        ts / 1000,
    );
    let resp = client
        .get(&url)
        .header("Cookie", session.cookie.clone())
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求登录确认失败: {e}")))?;
    let cookie = collect_cookies(&resp);
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取登录确认响应失败: {e}")))?;
    log::info!("cloud-drive: qrbdusslogin resp (truncated): {}", &text[..text.len().min(300)]);

    for part in cookie.split("; ") {
        if let Some((name, value)) = part.split_once('=') {
            if name == "BDUSS" && !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }
    Err(AppError::CloudDrive("扫码已确认，但未能从百度获取 BDUSS（请重试）".into()))
}

/// Poll the login state for a QR code. On success the session is stored and
/// `bduss` is returned in the payload.
///
/// One call runs the unicast loop internally (each pass waits ~30s, as the
/// endpoint returns once per wait cycle); the frontend calls it once and shows
/// a "waiting" state until it returns.
#[tauri::command]
pub async fn cloud_drive_baidu_qrlogin(
    app: AppHandle,
    sign: String,
) -> Result<BaiduQrStatus, AppError> {
    let session = QR_SESSION
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::CloudDrive("请先获取二维码".into()))?;
    // Unicast waits ~30s per pass when not yet scanned; give each pass 40s.
    let client = passport_client(40)?;

    // Tolerate several known response shapes:
    //   tangram_guid_xxx({"errno":0,"channel_id":"...","channel_v":"{\"status\":0,\"v\":\"<bduss>\",\"u\":\"\"}"})
    //   tangram_guid_xxx({"errno":1})                                   -> not scanned yet
    //   empty body (endpoint silent)                                    -> keep waiting
    for pass in 0..2 {
        let ts = now_ms();
        let url = format!(
            "{PASSPORT_BASE}/channel/unicast?channel_id={sign}&qrloginfrom=pc&gid={}&callback={}&apiver=v3&tt={ts}&tpl=netdisk&_={ts}",
            session.gid, session.callback,
        );
        let resp = client
            .get(&url)
            .header("Cookie", session.cookie.clone())
            .send()
            .await;
        let text = match resp {
            Ok(r) => r
                .text()
                .await
                .map_err(|e| AppError::CloudDrive(format!("读取登录状态响应失败: {e}")))?,
            Err(e) => {
                log::info!("cloud-drive: unicast pass #{pass} request error ({e}); keep waiting");
                continue;
            }
        };
        log::info!("cloud-drive: unicast pass #{pass} resp (truncated): {}", &text[..text.len().min(400)]);

        let json = extract_jsonp(&text);
        let Ok(raw) = json.parse::<serde_json::Value>() else {
            continue; // empty / not JSON yet
        };
        let errno = raw.get("errno").and_then(|v| v.as_i64()).unwrap_or(-1);
        if errno == 0 {
            let channel_v = raw.get("channel_v").and_then(|v| v.as_str()).unwrap_or("");
            if !channel_v.is_empty() {
                if let Ok(cv) = serde_json::from_str::<serde_json::Value>(channel_v) {
                    let status = cv.get("status").and_then(|v| v.as_i64()).unwrap_or(1);
                    let v = cv.get("v").and_then(|v| v.as_str()).unwrap_or("");
                    if status == 0 && !v.is_empty() {
                        let bduss = qrbdusslogin(&client, &session, v).await?;
                        save_cred(&app, &BaiduCredential { bduss: bduss.clone(), ptoken: String::new(), stoken: String::new() })?;
                        return Ok(BaiduQrStatus { status: "ok".into(), bduss: Some(bduss) });
                    }
                }
            }
            // errno=0 without a confirmed ticket -> scanned but not confirmed yet.
            return Ok(BaiduQrStatus { status: "waiting".into(), bduss: None });
        }
        if errno == 2 || errno == -6 {
            return Ok(BaiduQrStatus { status: "expired".into(), bduss: None });
        }
        // errno==1 (not scanned): loop again.
    }
    Ok(BaiduQrStatus { status: "waiting".into(), bduss: None })
}

/// Extract the JSON payload from a JSONP response (`callback(...)`).
fn extract_jsonp(text: &str) -> String {
    let s = text.trim();
    if s.is_empty() {
        return String::new();
    }
    let start = s.find('(').map(|i| i + 1).unwrap_or(0);
    let end = s.rfind(')').unwrap_or(s.len());
    if start > 0 && end > start {
        s[start..end].to_string()
    } else {
        s.to_string()
    }
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
