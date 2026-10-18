//! Aliyun share driver — port of alist `drivers/aliyundrive_share`.
//!
//! - token: `POST https://auth.alipan.com/v2/account/token` (refresh_token →
//!   access_token; the refresh_token itself rotates).
//! - share token: `POST https://api.alipan.com/v2/share_link/get_share_token`
//!   (share_id + optional share_pwd → share_token).
//! - list: `POST https://api.alipan.com/adrive/v3/file/list` with
//!   `x-share-token` + `X-Canary` headers (web-app variant; no content_hash).
//! - download: `POST https://api.alipan.com/v2/file/get_share_link_download_url`
//!   with `Referer: https://www.alipan.com/`.

use serde::{Deserialize, Serialize};

use crate::commands::aliyun_open::alist_file_to_drive;
use crate::error::AppError;
use crate::database::Database;

const ALIYUN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const CANARY_HEADER_VALUE: &str = "client=web,app=share,version=v2.3.1";

/// Aliyun share addition (persisted in `cloud_storages.addition`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliyunShareAddition {
    /// Refresh token of the alipan account (needed for access_token).
    pub refresh_token: String,
    /// Full share URL, e.g. `https://www.alipan.com/s/<code>?pwd=<pwd>`.
    #[serde(default)]
    pub share_url: String,
    /// Share id parsed from the URL.
    #[serde(default)]
    pub share_id: String,
    /// Optional share password.
    #[serde(default)]
    pub share_pwd: String,
    // ---- runtime cache (persisted so rotated tokens survive) ----
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_expires_at: u64,
    #[serde(default)]
    pub share_token: String,
    #[serde(default)]
    pub share_token_expires_at: u64,
    /// drive_id learned from the first list item.
    #[serde(default)]
    pub drive_id: String,
}

impl AliyunShareAddition {
    pub fn parse(json: &str) -> Result<Self, AppError> {
        serde_json::from_str(json)
            .map_err(|e| AppError::CloudDrive(format!("解析驱动配置失败: {e}")))
    }
    fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|e| AppError::CloudDrive(format!("序列化驱动配置失败: {e}")))
    }
    /// Extract share_id / share_pwd from a raw share URL.
    /// Accepts `https://www.alipan.com/s/<code>?pwd=<pwd>` and variants.
    pub fn parse_share_url(url: &str) -> (String, String) {
        let u = url.trim().trim_end_matches('#');
        // strip fragment entirely (a markdown suffix may follow `#`)
        let u = u.split('#').next().unwrap_or(u);
        let mut share_id = String::new();
        let mut share_pwd = String::new();
        if let Some(idx) = u.find("/s/") {
            let after = &u[idx + 3..];
            let (code, query) = match after.split_once('?') {
                Some((c, q)) => (c, Some(q)),
                None => (after, None),
            };
            share_id = code.trim_end_matches('/').to_string();
            if let Some(q) = query {
                for pair in q.split('&') {
                    if let Some((k, v)) = pair.split_once('=') {
                        if k.eq_ignore_ascii_case("pwd") || k.eq_ignore_ascii_case("password") {
                            share_pwd = v.to_string();
                        }
                    }
                }
            }
        }
        (share_id, share_pwd)
    }
}

pub struct AliyunShareDriver {
    pub id: i64,
    pub addition: AliyunShareAddition,
}

impl AliyunShareDriver {
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

    /// access_token via auth.alipan.com/v2/account/token.
    pub async fn ensure_access_token(&mut self, db: &Database) -> Result<String, AppError> {
        if !self.addition.access_token.is_empty()
            && self.addition.token_expires_at > Self::now()
        {
            return Ok(self.addition.access_token.clone());
        }
        let client = Self::http_client()?;
        let resp = client
            .post("https://auth.alipan.com/v2/account/token")
            .json(&serde_json::json!({
                "refresh_token": self.addition.refresh_token,
                "grant_type": "refresh_token",
            }))
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("刷新阿里 access_token 失败: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析阿里 token 响应失败: {e}")))?;
        let access = json["access_token"]
            .as_str()
            .ok_or_else(|| {
                let msg = json["message"].as_str().unwrap_or("未知错误");
                AppError::CloudDrive(format!("阿里分享刷新 token 失败：{msg}"))
            })?
            .to_string();
        if let Some(new_refresh) = json["refresh_token"].as_str() {
            if !new_refresh.is_empty() {
                self.addition.refresh_token = new_refresh.to_string();
            }
        }
        let expires_in = json["expires_in"].as_u64().unwrap_or(25 * 24 * 3600);
        self.addition.access_token = access.clone();
        self.addition.token_expires_at = Self::now() + expires_in;
        self.persist(db).await?;
        Ok(access)
    }

    /// share_token via get_share_token; refreshes when expired.
    pub async fn ensure_share_token(&mut self, db: &Database) -> Result<String, AppError> {
        if !self.addition.share_token.is_empty()
            && self.addition.share_token_expires_at > Self::now()
        {
            return Ok(self.addition.share_token.clone());
        }
        let client = Self::http_client()?;
        let mut body = serde_json::json!({ "share_id": self.addition.share_id });
        if !self.addition.share_pwd.is_empty() {
            body["share_pwd"] = serde_json::Value::String(self.addition.share_pwd.clone());
        }
        let resp = client
            .post("https://api.alipan.com/v2/share_link/get_share_token")
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("获取分享 token 失败: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析分享 token 响应失败: {e}")))?;
        let share_token = json["share_token"]
            .as_str()
            .ok_or_else(|| {
                let msg = json["message"].as_str().unwrap_or("未知错误");
                AppError::CloudDrive(format!("获取分享 token 失败：{msg}"))
            })?
            .to_string();
        let expires_in = json["expires_in"].as_u64().unwrap_or(2 * 3600);
        self.addition.share_token = share_token.clone();
        self.addition.share_token_expires_at = Self::now() + expires_in;
        self.persist(db).await?;
        Ok(share_token)
    }

    /// List children of `file_id` (`root`/empty = share root).
    pub async fn list(
        &mut self,
        db: &Database,
        file_id: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let access = self.ensure_access_token(db).await?;
        let share_token = self.ensure_share_token(db).await?;
        let client = Self::http_client()?;
        let parent = if file_id.is_empty() || file_id == "root" {
            "root".to_string()
        } else {
            file_id.to_string()
        };
        const MAX_PAGES: usize = 100;
        let mut out = Vec::new();
        let mut marker = String::new();
        for _ in 0..MAX_PAGES {
            let resp = client
                .post("https://api.alipan.com/adrive/v3/file/list")
                .header("Authorization", format!("Bearer\t{access}"))
                .header("x-share-token", &share_token)
                .header("X-Canary", CANARY_HEADER_VALUE)
                .json(&serde_json::json!({
                    "share_id": self.addition.share_id,
                    "parent_file_id": parent,
                    "limit": 200,
                    "marker": marker,
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求阿里分享列表失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析阿里分享列表失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "阿里分享列表失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            let items = json["items"].as_array().cloned().unwrap_or_default();
            for item in &items {
                // The share API reports the actual drive that hosts the share
                // on each item; remember it for the download call.
                if let Some(did) = item["drive_id"].as_str() {
                    if !did.is_empty() && self.addition.drive_id.is_empty() {
                        self.addition.drive_id = did.to_string();
                    }
                }
                out.push(alist_file_to_drive(item));
            }
            marker = json["next_marker"].as_str().unwrap_or("").to_string();
            if marker.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    /// Resolve CDN download URLs for the selected share files.
    pub async fn get_dlinks(
        &mut self,
        db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let access = self.ensure_access_token(db).await?;
        let share_token = self.ensure_share_token(db).await?;
        let mut drive_id = self.addition.drive_id.clone();
        if drive_id.is_empty() {
            // Files browsed in the current session carry the share drive id;
            // fall back to learning it from one root listing.
            drive_id = files
                .iter()
                .find(|f| !f.drive_id.is_empty())
                .map(|f| f.drive_id.clone())
                .unwrap_or_default();
        }
        if drive_id.is_empty() {
            self.list(db, "root").await?;
            drive_id = self.addition.drive_id.clone();
        }
        if drive_id.is_empty() {
            return Err(AppError::CloudDrive(
                "阿里分享 drive_id 为空，请先浏览一次分享目录".into(),
            ));
        }
        let client = Self::http_client()?;
        let mut result = Vec::new();
        for file in files {
            if file.isdir || file.id.is_empty() {
                continue;
            }
            let resp = client
                .post("https://api.alipan.com/v2/file/get_share_link_download_url")
                .header("Authorization", format!("Bearer\t{access}"))
                .header("x-share-token", &share_token)
                .header("X-Canary", CANARY_HEADER_VALUE)
                .header("Referer", "https://www.alipan.com/")
                .json(&serde_json::json!({
                    "drive_id": drive_id,
                    "file_id": file.id,
                    "expire_sec": 600,
                    "share_id": self.addition.share_id,
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
            let url = json["download_url"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| AppError::CloudDrive(format!("阿里分享未返回 {} 的下载地址", file.name)))?;
            // The CDN requires the alipan Referer.
            let headers = vec!["Referer: https://www.alipan.com/".to_string()];
            result.push((file.clone(), url, headers));
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何阿里分享文件的下载地址".into()));
        }
        Ok(result)
    }
}

