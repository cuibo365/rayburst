//! Aliyun Drive open-token QR login (xiaoya-style dual channel).
//!
//! Channels (selected at scan time; the chosen channel's refresh endpoint is
//! stored alongside the token so runtime refresh uses the same issuer):
//! - `messense`: public hosted OAuth (`aliyundrive-oauth.messense.me`)
//! - `xiaoya`:   xiaoya.pro hosted OAuth (`auth.xiaoya.pro/api/ali_open/...`)
//!
//! Flow: `aliyun_qr_create` → render the QR code (frontend) → poll
//! `aliyun_qr_status` (Aliyun official `openapi.aliyundrive.com/oauth/qrcode`
//! endpoint, shared by both channels) → on `LoginSuccess` exchange the
//! authCode via `aliyun_qr_exchange` → refresh_token + its refresh endpoint.

use crate::error::AppError;

const MESSENSE_QR: &str = "https://aliyundrive-oauth.messense.me/oauth/authorize/qrcode";
const MESSENSE_TOKEN: &str = "https://aliyundrive-oauth.messense.me/oauth/access_token";
const XIAOYA_QR: &str = "http://auth.xiaoya.pro/api/ali_open/qr";
const XIAOYA_TOKEN: &str = "http://auth.xiaoya.pro/api/ali_open/refresh";
const STATUS_POLL: &str = "https://openapi.aliyundrive.com/oauth/qrcode/";

/// Valid channel names.
pub const CHANNEL_MESSENSE: &str = "messense";
pub const CHANNEL_XIAOYA: &str = "xiaoya";

/// QR login session handed to the frontend for display/polling.
#[derive(serde::Serialize)]
pub struct AliyunQrCreated {
    pub sid: String,
    /// QR content; render it with a QR encoder.
    pub qr_code_url: String,
    pub qrcode_svg: String,
}

/// Poll result. `state`: success / pending / expired / error.
#[derive(serde::Serialize)]
pub struct AliyunQrStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Exchanged token plus the refresh endpoint that issued it (the frontend
/// stores this as `oauth_token_url` so runtime refresh stays in-channel).
#[derive(serde::Serialize)]
pub struct AliyunQrToken {
    pub refresh_token: String,
    pub token_url: String,
}

fn http_client() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
}

fn render_qr_svg(content: &str) -> Result<String, AppError> {
    let code = qrcode::QrCode::new(content.as_bytes())
        .map_err(|e| AppError::CloudDrive(format!("生成二维码失败: {e}")))?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .build())
}

/// Step 1: create a QR login session on the selected channel.
/// Returns the Aliyun sid + QR content (SVG) for the frontend.
#[tauri::command]
pub async fn aliyun_qr_create(channel: String) -> Result<AliyunQrCreated, AppError> {
    let client = http_client()?;
    let (sid, qr_code_url): (String, String) = match channel.as_str() {
        CHANNEL_MESSENSE => {
            let resp = client
                .post(MESSENSE_QR)
                .json(&serde_json::json!({
                    "scopes": ["user:base", "file:all:read", "file:all:write"],
                    "width": 300,
                    "height": 300,
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 messense 二维码失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析 messense 二维码响应失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "messense 二维码生成失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            let sid = json["sid"]
                .as_str()
                .ok_or_else(|| AppError::CloudDrive("messense 二维码响应缺少 sid".into()))?
                .to_string();
            let url = format!("https://www.aliyundrive.com/o/oauth/authorize?sid={sid}");
            (sid, url)
        }
        CHANNEL_XIAOYA => {
            let resp = client
                .get(XIAOYA_QR)
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 xiaoya 二维码失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析 xiaoya 二维码响应失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "xiaoya 二维码生成失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            let sid = json["sid"]
                .as_str()
                .ok_or_else(|| AppError::CloudDrive("xiaoya 二维码响应缺少 sid".into()))?
                .to_string();
            let url = format!("https://www.aliyundrive.com/o/oauth/authorize?sid={sid}");
            (sid, url)
        }
        other => {
            return Err(AppError::CloudDrive(format!(
                "不支持的扫码通道：{other}（可选 messense / xiaoya）"
            )));
        }
    };
    let qrcode_svg = render_qr_svg(&qr_code_url)?;
    Ok(AliyunQrCreated {
        sid,
        qr_code_url,
        qrcode_svg,
    })
}

/// Step 2: poll the scan status via the Aliyun official endpoint (shared by
/// both channels). On `LoginSuccess` returns the authCode for exchange.
#[tauri::command]
pub async fn aliyun_qr_status(sid: String) -> Result<AliyunQrStatus, AppError> {
    let client = http_client()?;
    let url = format!("{STATUS_POLL}{sid}/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("查询扫码状态失败: {e}")))?;
    let status = resp.status();
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析扫码状态响应失败: {e}")))?;
    if !status.is_success() {
        return Err(AppError::CloudDrive(format!(
            "查询扫码状态失败（HTTP {status}）：{}",
            json.get("message").and_then(|m| m.as_str()).unwrap_or("")
        )));
    }
    let st = json["status"].as_str().unwrap_or("");
    match st {
        "LoginSuccess" => {
            let code = json["authCode"]
                .as_str()
                .ok_or_else(|| AppError::CloudDrive("扫码成功但响应缺少 authCode".into()))?
                .to_string();
            Ok(AliyunQrStatus {
                state: "success".into(),
                auth_code: Some(code),
                message: None,
            })
        }
        "Expired" => Ok(AliyunQrStatus {
            state: "expired".into(),
            auth_code: None,
            message: Some("二维码已过期".into()),
        }),
        _ => Ok(AliyunQrStatus {
            state: "pending".into(),
            auth_code: None,
            message: None,
        }),
    }
}

/// Step 3: exchange the authCode for a refresh_token on the selected channel.
/// Returns the refresh_token plus the channel's refresh endpoint (stored as
/// `oauth_token_url`; runtime refresh must use the same issuer).
#[tauri::command]
pub async fn aliyun_qr_exchange(
    channel: String,
    auth_code: String,
) -> Result<AliyunQrToken, AppError> {
    let client = http_client()?;
    let (token_url, json): (String, serde_json::Value) = match channel.as_str() {
        CHANNEL_MESSENSE => {
            let resp = client
                .post(MESSENSE_TOKEN)
                .json(&serde_json::json!({
                    "grant_type": "authorization_code",
                    "code": auth_code,
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 messense 换 token 失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析 messense 换 token 响应失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "messense 换 token 失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            (MESSENSE_TOKEN.to_string(), json)
        }
        CHANNEL_XIAOYA => {
            let resp = client
                .post(XIAOYA_TOKEN)
                .json(&serde_json::json!({
                    "code": auth_code,
                    "grant_type": "authorization_code",
                    "client_id": "",
                    "client_secret": "",
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 xiaoya 换 token 失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析 xiaoya 换 token 响应失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "xiaoya 换 token 失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            (XIAOYA_TOKEN.to_string(), json)
        }
        other => {
            return Err(AppError::CloudDrive(format!(
                "不支持的扫码通道：{other}（可选 messense / xiaoya）"
            )));
        }
    };
    let refresh_token = json["refresh_token"]
        .as_str()
        .ok_or_else(|| {
            let detail = json["message"]
                .as_str()
                .or_else(|| json["error"].as_str())
                .unwrap_or("响应中无 refresh_token 字段");
            AppError::CloudDrive(format!("换 token 响应缺少 refresh_token：{detail}"))
        })?
        .to_string();
    Ok(AliyunQrToken {
        refresh_token,
        token_url,
    })
}
