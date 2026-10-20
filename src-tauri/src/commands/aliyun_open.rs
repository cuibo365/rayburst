//! Aliyun Drive (open platform) driver — port of alist `drivers/aliyundrive_open`.
//!
//! - token refresh: hosted renew endpoint chosen by the QR channel
//!   (messense / xiaoya), with a bare refresh_token).
//! - drive info: `POST /adrive/v1.0/user/getDriveInfo` → `default_drive_id`.
//! - list: `POST /adrive/v1.0/openFile/list` (marker pagination, limit 200).
//! - download: `POST /adrive/v1.0/openFile/getDownloadUrl` → CDN url.
//!
//! File objects carry `content_hash` (SHA1 hex); the *To115 drivers reuse it
//! as the 115 rapid-upload QuickID.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::database::Database;

pub const ALIYUN_API: &str = "https://openapi.alipan.com";
const ALIYUN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Extract the `sub` claim from an Alipan JWT (alist `getSub`); used to guard
/// refresh-token rotation against cross-account substitution.
fn jwt_sub(token: &str) -> Option<String> {
    use base64::Engine;
    let seg = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(seg)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("sub")?
        .as_str()
        .map(|s| s.to_string())
}

pub fn default_root() -> String {
    "root".into()
}

/// alist `DriveType` default (meta.go): `resource` = non-backup files.
pub fn default_drive_type() -> String {
    "resource".into()
}

/// Aliyun open addition (persisted in `cloud_storages.addition`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliyunOpenAddition {
    pub refresh_token: String,
    /// Browse root; `root` = drive root.
    #[serde(default = "default_root")]
    pub root_folder_id: String,
    /// Alipan drive: `default` / `resource` / `backup` (alist picks the
    /// matching `{type}_drive_id` from getDriveInfo).
    #[serde(default = "default_drive_type")]
    pub drive_type: String,
    /// Sort key: name / size / updated_at / created_at (empty = default).
    #[serde(default)]
    pub order_by: String,
    /// ASC / DESC.
    #[serde(default)]
    pub order_direction: String,
    /// QR login channel (`messense` / `xiaoya`) recorded at scan time;
    /// `ensure_access_token` selects the channel's refresh endpoint from it.
    #[serde(default)]
    pub qr_channel: String,
    // ---- runtime token cache (persisted so a renewed refresh_token survives) ----
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_expires_at: u64,
    #[serde(default)]
    pub drive_id: String,
}

impl AliyunOpenAddition {
    pub fn parse(json: &str) -> Result<Self, AppError> {
        serde_json::from_str(json)
            .map_err(|e| AppError::CloudDrive(format!("解析驱动配置失败: {e}")))
    }
    fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|e| AppError::CloudDrive(format!("序列化驱动配置失败: {e}")))
    }
}

pub struct AliyunOpenDriver {
    pub id: i64,
    pub addition: AliyunOpenAddition,
    /// Embedded drivers (aliyun_drive_115's open_driver()) share this storage
    /// id but their addition is a different type (contains the 115 cookie);
    /// persisting AliyunOpenAddition's JSON here would wipe cookie/user_key/
    /// dir_id. When true, token rotation is merged back by the outer driver.
    pub no_persist: bool,
}

impl AliyunOpenDriver {
    fn http_client() -> Result<reqwest::Client, AppError> {
        reqwest::Client::builder()
            .user_agent(ALIYUN_UA)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub async fn persist(&self, db: &Database) -> Result<(), AppError> {
        db.update_storage_addition(self.id, &self.addition.to_json()?)
            .await?;
        Ok(())
    }

    pub async fn set_status(&self, db: &Database, status: &str) -> Result<(), AppError> {
        db.set_storage_status(self.id, status).await
    }

    /// Refresh → access_token; persists a newly rotated refresh_token.
    pub async fn ensure_access_token(&mut self, db: &Database) -> Result<String, AppError> {
        if !self.addition.access_token.is_empty()
            && self.addition.token_expires_at > Self::now()
        {
            return Ok(self.addition.access_token.clone());
        }
        // 刷新端点由扫码通道决定（messense / xiaoya），不再支持自定义托管。
        let client = Self::http_client()?;
        let json: serde_json::Value;
        match self.addition.qr_channel.as_str() {
            "messense" => {
                // messense hosted OAuth: standard token endpoint, no client fields.
                let resp = client
                    .post("https://aliyundrive-oauth.messense.me/oauth/access_token")
                    .json(&serde_json::json!({
                        "grant_type": "refresh_token",
                        "refresh_token": self.addition.refresh_token,
                    }))
                    .send()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("调用 messense 刷新接口失败: {e}")))?;
                json = resp
                    .json()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("解析 messense 刷新响应失败: {e}")))?;
            }
            "xiaoya" => {
                // xiaoya hosted OAuth: same shape as its auth_code exchange
                // (empty client fields included).
                let resp = client
                    .post("http://auth.xiaoya.pro/api/ali_open/refresh")
                    .json(&serde_json::json!({
                        "client_id": "",
                        "client_secret": "",
                        "grant_type": "refresh_token",
                        "refresh_token": self.addition.refresh_token,
                    }))
                    .send()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("调用 xiaoya 刷新接口失败: {e}")))?;
                json = resp
                    .json()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("解析 xiaoya 刷新响应失败: {e}")))?;
            }
            other => {
                return Err(AppError::CloudDrive(format!(
                    "阿里云盘扫码通道未设置或无效（{other:?}），请重新编辑该存储并扫码获取 refresh_token"
                )));
            }
        }
        let access = json["access_token"]
            .as_str()
            .ok_or_else(|| {
                let detail = json["text"]
                    .as_str()
                    .or_else(|| json["message"].as_str())
                    .or_else(|| json["error"].as_str())
                    .unwrap_or("响应中无 access_token 字段");
                AppError::CloudDrive(format!("阿里云盘刷新响应缺少 access_token：{detail}"))
            })?
            .to_string();
        if let Some(new_refresh) = json["refresh_token"].as_str() {
            if !new_refresh.is_empty() {
                // alist guards the rotation with a JWT `sub` match: refuse a
                // refresh_token that belongs to a different account.
                match (jwt_sub(self.addition.refresh_token.as_str()), jwt_sub(new_refresh)) {
                    (Some(cur), Some(new)) if cur == new => {
                        self.addition.refresh_token = new_refresh.to_string();
                    }
                    _ => {}
                }
            }
        }
        let expires_in = json["expires_in"].as_u64().unwrap_or(25 * 24 * 3600);
        self.addition.access_token = access.clone();
        self.addition.token_expires_at = Self::now() + expires_in;
        if !self.no_persist {
            self.persist(db).await?;
        }
        Ok(access)
    }

    /// Get the default drive id (cached).
    pub async fn ensure_drive_id(
        &mut self,
        db: &Database,
        access_token: &str,
    ) -> Result<String, AppError> {
        if !self.addition.drive_id.is_empty() {
            return Ok(self.addition.drive_id.clone());
        }
        let client = Self::http_client()?;
        let resp = client
            .post(format!("{ALIYUN_API}/adrive/v1.0/user/getDriveInfo"))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("获取阿里云盘信息失败: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析阿里云盘信息失败: {e}")))?;
        // alist picks `{drive_type}_drive_id` (default / resource / backup),
        // falling back to the plain default when the account has no such drive.
        let drive_type = self.addition.drive_type.trim();
        let drive_id = json[format!("{drive_type}_drive_id")]
            .as_str()
            .or_else(|| json["default_drive_id"].as_str())
            .or_else(|| json["drive_id"].as_str())
            .ok_or_else(|| {
                AppError::CloudDrive(format!(
                    "阿里云盘信息响应缺少 drive_id: {}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                ))
            })?
            .to_string();
        self.addition.drive_id = drive_id.clone();
        self.persist(db).await?;
        Ok(drive_id)
    }

    /// List children of `file_id` (`root`/empty = configured root).
    pub async fn list(
        &mut self,
        db: &Database,
        file_id: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let token = self.ensure_access_token(db).await?;
        let drive_id = self.ensure_drive_id(db, &token).await?;
        let client = Self::http_client()?;
        let parent = if file_id.is_empty() || file_id == "root" {
            let root = self.addition.root_folder_id.trim();
            if root.is_empty() {
                "root".to_string()
            } else {
                root.to_string()
            }
        } else {
            file_id.to_string()
        };
        const MAX_PAGES: usize = 100;
        let mut out = Vec::new();
        let mut marker = String::new();
        for _ in 0..MAX_PAGES {
            let mut body = serde_json::json!({
                "drive_id": drive_id,
                "limit": 200,
                "parent_file_id": parent,
                "marker": marker,
            });
            if !self.addition.order_by.is_empty() {
                body["order_by"] = serde_json::Value::String(self.addition.order_by.clone());
            }
            if !self.addition.order_direction.is_empty() {
                body["order_direction"] =
                    serde_json::Value::String(self.addition.order_direction.clone());
            }
            let resp = client
                .post(format!("{ALIYUN_API}/adrive/v1.0/openFile/list"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求阿里云盘列表失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析阿里云盘列表失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "阿里云盘列表失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            let items = json["items"].as_array().cloned().unwrap_or_default();
            for item in items {
                out.push(alist_file_to_drive(&item));
            }
            marker = json["next_marker"].as_str().unwrap_or("").to_string();
            if marker.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// Resolve CDN download URLs for the selected files.
    pub async fn get_dlinks(
        &mut self,
        db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let token = self.ensure_access_token(db).await?;
        let drive_id = self.ensure_drive_id(db, &token).await?;
        let client = Self::http_client()?;
        let mut result = Vec::new();
        for file in files {
            if file.isdir || file.id.is_empty() {
                continue;
            }
            let resp = client
                .post(format!("{ALIYUN_API}/adrive/v1.0/openFile/getDownloadUrl"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "drive_id": drive_id,
                    "file_id": file.id,
                    "expire_sec": 14400,
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 {} 下载地址失败: {e}", file.name)))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析 {} 下载响应失败: {e}", file.name)))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "获取 {} 下载地址失败（HTTP {status}）：{}",
                    file.name,
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            let url = json["url"]
                .as_str()
                .map(str::to_string)
                .or_else(|| json["streamsUrl"]["jpeg"].as_str().map(str::to_string))
                .ok_or_else(|| AppError::CloudDrive(format!("阿里云盘未返回 {} 的下载地址", file.name)))?;
            // The CDN url is pre-signed; alist sends no extra headers.
            result.push((file.clone(), url, vec![]));
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何阿里云盘文件的下载地址".into()));
        }
        Ok(result)
    }
}

/// Map an aliyun open list item to a `CloudDriveFile`.
///
/// Convention shared with quark/m115: `path` carries the driver's folder key
/// (here the file_id) — the frontend uses it as the browse key and
/// `expand_folders` recurses with it.
pub(crate) fn alist_file_to_drive(item: &serde_json::Value) -> crate::commands::CloudDriveFile {
    let file_id = item["file_id"].as_str().unwrap_or("").to_string();
    let name = item["name"].as_str().unwrap_or("").to_string();
    let isdir = item["type"].as_str() == Some("folder");
    crate::commands::CloudDriveFile {
        fs_id: 0,
        name,
        size: item["size"].as_u64().unwrap_or(0),
        path: file_id.clone(),
        isdir,
        mtime: parse_aliyun_time(item["updated_at"].as_str().unwrap_or("")),
        category: 0,
        id: file_id,
        rel_path: String::new(),
        pdir_fid: item["parent_file_id"].as_str().unwrap_or("").to_string(),
        share_fid_token: String::new(),
        pickcode: String::new(),
        sha1: item["content_hash"].as_str().unwrap_or("").to_string(),
        drive_id: item["drive_id"].as_str().unwrap_or("").to_string(),
    }
}

/// Parse an aliyun RFC3339 timestamp (e.g. `2024-01-01T12:00:00.000Z`).
pub(crate) fn parse_aliyun_time(s: &str) -> u64 {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp().max(0) as u64;
    }
    0
}
