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
    /// Where the share file is copied into the own drive before downloading
    /// (`root` = drive root; otherwise a folder file_id).
    #[serde(default = "default_copy_dir")]
    pub open_copy_dir: String,
    // ---- runtime cache (persisted so rotated tokens survive) ----
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_expires_at: u64,
    #[serde(default)]
    pub share_token: String,
    #[serde(default)]
    pub share_token_expires_at: u64,
    /// drive_id learned from the first list item (the share's hosting drive).
    #[serde(default)]
    pub drive_id: String,
    /// The user's own drive id (copy target); learned from the copy response
    /// or the own-drive listing.
    #[serde(default)]
    pub own_drive_id: String,
}

fn default_copy_dir() -> String {
    "root".into()
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
    /// Transient (提取分享) drivers must not write their in-memory share
    /// fields back into the source storage; account-scoped fields
    /// (refresh_token rotation) are still merged back.
    pub no_persist: bool,
    /// Source storage's original addition (transient only); used to merge
    /// account-scoped writes without touching share fields.
    pub original: Option<AliyunShareAddition>,
}

impl AliyunShareDriver {
    /// Transient driver for 提取分享: borrows the source storage's
    /// credentials (refresh_token, access_token cache, own_drive_id) but
    /// replaces the share fields with the just-entered URL.
    ///
    /// Share-scoped caches (`share_token`, `drive_id`) are cleared: a cached
    /// token belongs to the source storage's share and would 400 against the
    /// new share_id ("share_id doesn't match share_token").
    pub fn transient(
        storage_id: i64,
        storage: &AliyunShareAddition,
        share_url: &str,
        passcode: &str,
    ) -> Self {
        let mut addition = storage.clone();
        let (sid, spwd) = AliyunShareAddition::parse_share_url(share_url);
        addition.share_url = share_url.to_string();
        addition.share_id = sid;
        addition.share_pwd = if spwd.is_empty() {
            passcode.to_string()
        } else {
            spwd
        };
        addition.share_token = String::new();
        addition.share_token_expires_at = 0;
        addition.drive_id = String::new();
        Self {
            id: storage_id,
            addition,
            no_persist: true,
            original: Some(storage.clone()),
        }
    }

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
        if let Some(orig) = &self.original {
            // transient（提取分享）：只回写账号级字段。refresh_token 每次
            // 刷新都会旋转且旧值立即作废，若不存回源存储，源存储（及共用
            // 同一 refresh_token 的其他驱动）会在下次刷新时失效。
            let mut saved = orig.clone();
            saved.refresh_token = self.addition.refresh_token.clone();
            saved.access_token = self.addition.access_token.clone();
            saved.token_expires_at = self.addition.token_expires_at;
            if !self.addition.own_drive_id.is_empty() {
                saved.own_drive_id = self.addition.own_drive_id.clone();
            }
            db.update_storage_addition(self.id, &saved.to_json()?).await?;
            return Ok(());
        }
        db.update_storage_addition(self.id, &self.addition.to_json()?)
            .await?;
        Ok(())
    }

    pub async fn set_status(&self, db: &Database, status: &str) -> Result<(), AppError> {
        if self.no_persist {
            return Ok(());
        }
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
        let parent = if file_id.is_empty() || file_id == "root" || file_id == "0" {
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

    /// List a directory of the user's own drive (web-domain only).
    ///
    /// Unlike the share listing, the own-drive `adrive/v3/file/list` items
    /// carry `content_hash` (SHA1) — this is how the *To115 drivers obtain
    /// the SHA1 for the 115 rapid upload without the open platform.
    pub async fn list_own_dir(
        &mut self,
        db: &Database,
        drive_id: &str,
        parent_file_id: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let access = self.ensure_access_token(db).await?;
        let client = Self::http_client()?;
        let parent = if parent_file_id.is_empty() || parent_file_id == "root" {
            "root".to_string()
        } else {
            parent_file_id.to_string()
        };
        const MAX_PAGES: usize = 100;
        let mut out = Vec::new();
        let mut marker = String::new();
        for _ in 0..MAX_PAGES {
            let resp = client
                .post("https://api.aliyundrive.com/adrive/v3/file/list")
                .header("Authorization", format!("Bearer\t{access}"))
                .header("X-Canary", "client=windows,app=adrive,version=v6.0.0")
                .json(&serde_json::json!({
                    "drive_id": drive_id,
                    "parent_file_id": parent,
                    "limit": 200,
                    "marker": marker,
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求阿里云盘目录列表失败: {e}")))?;
            let status = resp.status();
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析阿里云盘目录列表失败: {e}")))?;
            if !status.is_success() {
                return Err(AppError::CloudDrive(format!(
                    "阿里云盘目录列表失败（HTTP {status}）：{}",
                    json.get("message").and_then(|m| m.as_str()).unwrap_or("")
                )));
            }
            let items = json["items"].as_array().cloned().unwrap_or_default();
            for item in &items {
                // Own-drive items carry content_hash (SHA1); the converter
                // picks it up into the `sha1` field.
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
    ///
    /// Flow (web-domain only, mirrors LinkSwift / the alipan web app):
    /// copy the share file into the user's own drive via `v2/file/copy`,
    /// then download it from the own drive via `v2/file/get_download_url`
    /// (the share-scoped `get_share_link_download_url` endpoint 410s on
    /// many files).
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
        let copy_dir = {
            let d = self.addition.open_copy_dir.trim();
            if d.is_empty() {
                "root".to_string()
            } else {
                d.to_string()
            }
        };
        let client = Self::http_client()?;
        // The copy request needs our own drive id up front.
        let own_drive = self.ensure_own_drive_id(&client, &access).await?;
        let mut result = Vec::new();
        for file in files {
            if file.isdir || file.id.is_empty() {
                continue;
            }
            // 1) copy the share file into the own drive
            let copy_json = self
                .copy_to_own(
                    &client,
                    &access,
                    &share_token,
                    &self.addition.share_id,
                    &drive_id,
                    &own_drive,
                    &copy_dir,
                    file,
                )
                .await?;
            // The 202 copy response carries the destination file id and
            // drive id at the top level (older replies use `tasks[0]`).
            let mut own_file_id = copy_json["file_id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_default();
            let mut own_file_drive = copy_json["drive_id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| own_drive.clone());
            if own_file_id.is_empty() {
                own_file_id = copy_json
                    .get("tasks")
                    .and_then(|t| t.as_array())
                    .and_then(|tasks| tasks.first())
                    .and_then(|task| task["file_id"].as_str())
                    .unwrap_or("")
                    .to_string();
            }
            // 2) otherwise poll the own-drive target dir for the copied file
            if own_file_id.is_empty() {
                let (fid, did) = self
                    .find_copied(&client, &access, &own_drive, &copy_dir, file)
                    .await?;
                own_file_id = fid;
                if !did.is_empty() {
                    own_file_drive = did;
                }
            }
            if own_file_drive.is_empty() || own_file_id.is_empty() {
                return Err(AppError::CloudDrive(format!(
                    "分享文件 {} 转存后未能在自己的云盘定位到目标文件",
                    file.name
                )));
            }
            // 3) download from the own drive (web-domain endpoint)
            let resp = client
                .post("https://api.alipan.com/v2/file/get_download_url")
                .header("Authorization", format!("Bearer {access}"))
                .header("X-Canary", "client=windows,app=adrive,version=v6.0.0")
                .header("Referer", "https://www.alipan.com/")
                .json(&serde_json::json!({
                    "drive_id": own_file_drive,
                    "file_id": own_file_id,
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 {} 下载地址失败: {e}", file.name)))?;
            let status = resp.status();
            // Read the body as text first so non-JSON replies (rate-limit /
            // risk-control HTML pages, empty bodies) surface their real
            // content instead of a bare "error decoding response body".
            let text = resp
                .text()
                .await
                .map_err(|e| AppError::CloudDrive(format!("读取 {} 下载响应失败: {e}", file.name)))?;
            if !status.is_success() {
                let body = text.chars().take(300).collect::<String>();
                return Err(AppError::CloudDrive(format!(
                    "获取 {} 下载地址失败（HTTP {status}）：{body}",
                    file.name
                )));
            }
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                AppError::CloudDrive(format!(
                    "解析 {} 下载响应失败: {e}（响应内容: {}）",
                    file.name,
                    text.chars().take(300).collect::<String>()
                ))
            })?;
            let url = json["url"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| AppError::CloudDrive(format!("阿里云盘未返回 {} 的下载地址", file.name)))?;
            // The CDN requires the alipan Referer.
            let headers = vec!["Referer: https://www.alipan.com/".to_string()];
            result.push((file.clone(), url, headers));
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何阿里分享文件的下载地址".into()));
        }
        Ok(result)
    }

    /// The user's own drive id (copy target), cached in `own_drive_id`.
    /// pub(crate): also used by `aliyun_share_115` (web-domain only, no open).
    pub(crate) async fn ensure_own_drive_id(
        &mut self,
        client: &reqwest::Client,
        access: &str,
    ) -> Result<String, AppError> {
        if !self.addition.own_drive_id.is_empty() {
            return Ok(self.addition.own_drive_id.clone());
        }
        // `user.aliyundrive.com/v2/user/get` is the only web-domain endpoint
        // that returns the full drive set (api.* hosts only expose
        // default_drive_id). Prefer the resource drive — that is the drive
        // the web app's "全部文件" view and user-provided folder ids live in.
        let resp = client
            .post("https://user.aliyundrive.com/v2/user/get")
            .header("Authorization", format!("Bearer {access}"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("获取自己的云盘信息失败: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::CloudDrive(format!("读取云盘信息响应失败: {e}")))?;
        if !status.is_success() {
            let body = text.chars().take(300).collect::<String>();
            return Err(AppError::CloudDrive(format!(
                "获取自己的云盘信息失败（HTTP {status}）：{body}"
            )));
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| AppError::CloudDrive(format!("解析云盘信息响应失败: {e}")))?;
        let drive_id = json["resource_drive_id"]
            .as_str()
            .or_else(|| json["default_drive_id"].as_str())
            .or_else(|| json["drive_id"].as_str())
            .ok_or_else(|| {
                AppError::CloudDrive(format!(
                    "云盘信息响应缺少 drive_id: {}",
                    text.chars().take(300).collect::<String>()
                ))
            })?
            .to_string();
        self.addition.own_drive_id = drive_id.clone();
        Ok(drive_id)
    }

    /// v2/file/copy: 把分享文件转存到自己的云盘（网页域 token）。
    ///
    /// The `share_id` field in the body is required (without it the API
    /// rejects with `InvalidParameterNotMatch.ShareId`).
    async fn copy_to_own(
        &self,
        client: &reqwest::Client,
        access: &str,
        share_token: &str,
        share_id: &str,
        share_drive: &str,
        own_drive: &str,
        to_parent: &str,
        file: &crate::commands::CloudDriveFile,
    ) -> Result<serde_json::Value, AppError> {
        let resp = client
            .post("https://api.alipan.com/v2/file/copy")
            .header("Authorization", format!("Bearer\t{access}"))
            .header("x-share-token", share_token)
            .header("X-Canary", CANARY_HEADER_VALUE)
            .json(&serde_json::json!({
                "drive_id": share_drive,
                "file_id": file.id,
                "share_id": share_id,
                "to_drive_id": own_drive,
                "to_parent_file_id": to_parent,
            }))
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("转存 {} 到云盘失败: {e}", file.name)))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::CloudDrive(format!("读取转存响应失败: {e}")))?;
        if !status.is_success() {
            let body = text.chars().take(300).collect::<String>();
            return Err(AppError::CloudDrive(format!(
                "转存 {} 失败（HTTP {status}）：{body}",
                file.name
            )));
        }
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| AppError::CloudDrive(format!("解析转存响应失败: {e}")))?;
        Ok(json)
    }

    /// Poll the own-drive copy dir until a file matching name+size appears.
    async fn find_copied(
        &self,
        client: &reqwest::Client,
        access: &str,
        own_drive: &str,
        copy_dir: &str,
        file: &crate::commands::CloudDriveFile,
    ) -> Result<(String, String), AppError> {
        // The copy is async; poll the target dir up to ~30s.
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let resp = client
                .post("https://api.alipan.com/v2/file/list")
                .header("Authorization", format!("Bearer {access}"))
                .json(&serde_json::json!({
                    "drive_id": own_drive,
                    "parent_file_id": copy_dir,
                    "limit": 200,
                    "marker": "",
                }))
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求自己的云盘目录失败: {e}")))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| AppError::CloudDrive(format!("读取云盘目录响应失败: {e}")))?;
            if !status.is_success() {
                let body = text.chars().take(300).collect::<String>();
                return Err(AppError::CloudDrive(format!(
                    "请求自己的云盘目录失败（HTTP {status}）：{body}"
                )));
            }
            let json: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| AppError::CloudDrive(format!("解析云盘目录响应失败: {e}")))?;
            let items = json["items"].as_array().cloned().unwrap_or_default();
            for item in items {
                if item["name"].as_str() == Some(file.name.as_str())
                    && item["size"].as_u64().unwrap_or(0) == file.size
                {
                    return Ok((
                        item["file_id"].as_str().unwrap_or("").to_string(),
                        item["drive_id"].as_str().unwrap_or("").to_string(),
                    ));
                }
            }
        }
        Err(AppError::CloudDrive(format!(
            "分享文件 {} 转存到云盘超时（30s 内未在目标目录找到）",
            file.name
        )))
    }
}

