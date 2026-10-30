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

use crate::commands::netdisk_http;
use crate::error::AppError;
use base64::Engine as _;

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
            let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
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
            let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
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
    let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
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
            let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
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
            let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
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

// ---------------------------------------------------------------------------
// Web-domain token QR login (alistgo official endpoints).
//
// Unlike the open-token flow above (OAuth clients), the *web* refresh_token
// (used by aliyun_share / aliyun_share_115) is obtained through Aliyun's
// passport web login. alistgo hosts both endpoints so no passport session is
// needed here:
//   create: GET  api.alistgo.com/alist/ali/qr           → {t, ck, codeContent}
//   poll:   POST api.alistgo.com/alist/ali/ck  {ck, t}  → qrCodeStatus;
//           on CONFIRMED the response's bizExt (base64, GBK-encoded JSON)
//           carries `pds_login_result.refreshToken` directly.
// ---------------------------------------------------------------------------

const ALISTGO_WEB_QR: &str = "https://api.alistgo.com/alist/ali/qr";
const ALISTGO_WEB_CK: &str = "https://api.alistgo.com/alist/ali/ck";

/// QR login session for the web-domain token flow.
#[derive(serde::Serialize)]
pub struct AliyunWebQrCreated {
    pub ck: String,
    pub t: String,
    pub qrcode_svg: String,
}

/// Poll result. `state`: success / pending / expired / error.
#[derive(serde::Serialize)]
pub struct AliyunWebQrStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Step 1: create a web-domain QR login session via alistgo.
#[tauri::command]
pub async fn aliyun_web_qr_create() -> Result<AliyunWebQrCreated, AppError> {
    let client = http_client()?;
    let resp = client
        .get(ALISTGO_WEB_QR)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求 alistgo 网页 token 二维码失败: {e}")))?;
    let status = resp.status();
    let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析 alistgo 网页 token 二维码响应失败: {e}")))?;
    if !status.is_success() {
        return Err(AppError::CloudDrive(format!(
            "alistgo 网页 token 二维码生成失败（HTTP {status}）：{}",
            json.get("message").and_then(|m| m.as_str()).unwrap_or("")
        )));
    }
    let data = &json["content"]["data"];
    let ck = data["ck"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("网页 token 二维码响应缺少 ck".into()))?
        .to_string();
    let t = data["t"].as_i64().map(|v| v.to_string()).ok_or_else(|| {
        AppError::CloudDrive("网页 token 二维码响应缺少 t".into())
    })?;
    let code = data["codeContent"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("网页 token 二维码响应缺少 codeContent".into()))?;
    let qrcode_svg = render_qr_svg(code)?;
    Ok(AliyunWebQrCreated { ck, t, qrcode_svg })
}

/// Step 2: poll the web-domain scan status. On success returns the web
/// refresh_token decoded from the response's bizExt (base64 → GBK → JSON).
#[tauri::command]
pub async fn aliyun_web_qr_status(ck: String, t: String) -> Result<AliyunWebQrStatus, AppError> {
    let client = http_client()?;
    let resp = client
        .post(ALISTGO_WEB_CK)
        .json(&serde_json::json!({ "ck": ck, "t": t }))
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("查询网页 token 扫码状态失败: {e}")))?;
    let status = resp.status();
    let json: serde_json::Value = netdisk_http::read_json(resp, "aliyun-qr")
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析网页 token 扫码状态响应失败: {e}")))?;
    if !status.is_success() {
        return Err(AppError::CloudDrive(format!(
            "查询网页 token 扫码状态失败（HTTP {status}）：{}",
            json.get("message").and_then(|m| m.as_str()).unwrap_or("")
        )));
    }
    let data = &json["content"]["data"];
    let st = data["qrCodeStatus"].as_str().unwrap_or("");
    match st {
        "CONFIRMED" => {
            let b64 = data["bizExt"].as_str().ok_or_else(|| {
                AppError::CloudDrive("扫码成功但响应缺少 bizExt".into())
            })?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| AppError::CloudDrive(format!("bizExt base64 解码失败: {e}")))?;
            let (text, _, _) = encoding_rs::GBK.decode(&bytes);
            let biz: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| AppError::CloudDrive(format!("bizExt JSON 解析失败: {e}")))?;
            let refresh_token = biz["pds_login_result"]["refreshToken"]
                .as_str()
                .ok_or_else(|| AppError::CloudDrive("bizExt 中缺少 refreshToken".into()))?
                .to_string();
            Ok(AliyunWebQrStatus {
                state: "success".into(),
                refresh_token: Some(refresh_token),
                message: None,
            })
        }
        "EXPIRED" => Ok(AliyunWebQrStatus {
            state: "expired".into(),
            refresh_token: None,
            message: Some("二维码已过期".into()),
        }),
        _ => Ok(AliyunWebQrStatus {
            state: "pending".into(),
            refresh_token: None,
            message: None,
        }),
    }
}
