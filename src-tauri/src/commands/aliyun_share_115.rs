//! Aliyun share → 115 driver ("阿里分享转115").
//!
//! The share list API does not expose `content_hash` (SHA1), so a file must
//! first be copied into the user's own Aliyun Drive (`v2/file/copy` with the
//! share token) — where the open list returns its SHA1 — then go through the
//! 115 rapid upload → downurl path (same as `aliyun_drive_115`). The copy is
//! free and lands in the user's drive (`open_copy_dir`); 115 flash-transfer
//! costs no cloud-download quota.

use serde::{Deserialize, Serialize};

use crate::commands::aliyun_open::{default_oauth_url, default_root, AliyunOpenAddition, AliyunOpenDriver};
use crate::commands::aliyun_share::{AliyunShareAddition, AliyunShareDriver};
use crate::error::AppError;
use crate::commands::m115_crypto;
use crate::database::Database;

const M115_DOWN_URL: &str = "https://proapi.115.com/app/chrome/downurl";

fn default_115_dir() -> String {
    "0".into()
}

fn default_copy_dir() -> String {
    "root".into()
}

/// Combined addition: Aliyun open (own drive) + share + 115 session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliyunShare115Addition {
    /// Refresh token of the user's own Aliyun Drive (copy target + SHA1 source).
    pub refresh_token: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default = "default_oauth_url")]
    pub oauth_token_url: String,
    /// Where the share file is copied into the own drive (`root` default).
    #[serde(default = "default_copy_dir")]
    pub open_copy_dir: String,
    /// Full share URL.
    #[serde(default)]
    pub share_url: String,
    /// Share id parsed from the URL.
    #[serde(default)]
    pub share_id: String,
    /// Optional share password.
    #[serde(default)]
    pub share_pwd: String,
    /// 115 web-session cookie.
    pub cookie: String,
    /// Optional 115 user key.
    #[serde(default)]
    pub user_key: String,
    /// 115 target dir for the rapid-uploaded file (`0` = drive root).
    #[serde(default = "default_115_dir")]
    pub dir_id: String,
    // ---- runtime caches (persisted) ----
    /// open-domain access_token (openapi.alipan.com; hosted/oauth refresh).
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_expires_at: u64,
    /// share-domain access_token (auth.alipan.com/v2/account/token).
    /// Kept separate from `access_token`: the two domains issue tokens that
    /// are NOT interchangeable.
    #[serde(default)]
    pub share_access_token: String,
    #[serde(default)]
    pub share_access_expires_at: u64,
    #[serde(default)]
    pub drive_id: String,
    #[serde(default)]
    pub share_token: String,
    #[serde(default)]
    pub share_token_expires_at: u64,
    #[serde(default)]
    pub user_id: i64,
}

impl AliyunShare115Addition {
    pub fn parse(json: &str) -> Result<Self, AppError> {
        serde_json::from_str(json)
            .map_err(|e| AppError::CloudDrive(format!("解析驱动配置失败: {e}")))
    }
    fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|e| AppError::CloudDrive(format!("序列化驱动配置失败: {e}")))
    }
    /// Extract the numeric UID from a 115 cookie (`UID=342098069_R2_…`).
    pub fn parse_uid(cookie: &str) -> i64 {
        for part in cookie.split(';') {
            let part = part.trim();
            if let Some(v) = part.strip_prefix("UID=") {
                if let Some(num) = v.split('_').next() {
                    if let Ok(n) = num.trim().parse::<i64>() {
                        return n;
                    }
                }
            }
        }
        0
    }
}

pub struct AliyunShare115Driver {
    pub id: i64,
    pub addition: AliyunShare115Addition,
}

impl AliyunShare115Driver {
    fn http_client(ua: &str) -> Result<reqwest::Client, AppError> {
        reqwest::Client::builder()
            .user_agent(ua)
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

    fn open_driver(&self) -> AliyunOpenDriver {
        AliyunOpenDriver {
            id: self.id,
            addition: AliyunOpenAddition {
                refresh_token: self.addition.refresh_token.clone(),
                client_id: self.addition.client_id.clone(),
                client_secret: self.addition.client_secret.clone(),
                root_folder_id: "root".into(),
                order_by: String::new(),
                order_direction: String::new(),
                oauth_token_url: self.addition.oauth_token_url.clone(),
                access_token: self.addition.access_token.clone(),
                token_expires_at: self.addition.token_expires_at,
                drive_id: self.addition.drive_id.clone(),
            },
        }
    }

    fn share_driver(&self) -> AliyunShareDriver {
        AliyunShareDriver {
            id: self.id,
            addition: AliyunShareAddition {
                refresh_token: self.addition.refresh_token.clone(),
                share_url: self.addition.share_url.clone(),
                share_id: self.addition.share_id.clone(),
                share_pwd: self.addition.share_pwd.clone(),
                access_token: self.addition.share_access_token.clone(),
                token_expires_at: self.addition.share_access_expires_at,
                share_token: self.addition.share_token.clone(),
                share_token_expires_at: self.addition.share_token_expires_at,
                drive_id: self.addition.drive_id.clone(),
            },
        }
    }

    fn sync_caches(&mut self, open: &AliyunOpenDriver, share: &AliyunShareDriver) {
        self.addition.access_token = open.addition.access_token.clone();
        self.addition.token_expires_at = open.addition.token_expires_at;
        self.addition.drive_id = open.addition.drive_id.clone();
        // refresh_token 会旋转：share 驱动最后刷新，取它的旋转值
        // （托管模式下 open/share 同源于 app 域 token，可互换）。
        self.addition.refresh_token = share.addition.refresh_token.clone();
        self.addition.share_access_token = share.addition.access_token.clone();
        self.addition.share_access_expires_at = share.addition.token_expires_at;
        self.addition.share_token = share.addition.share_token.clone();
        self.addition.share_token_expires_at = share.addition.share_token_expires_at;
    }

    /// List the share tree.
    pub async fn list(
        &mut self,
        db: &Database,
        file_id: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let mut share = self.share_driver();
        let r = share.list(db, file_id).await;
        let open = self.open_driver();
        self.sync_caches(&open, &share);
        if let Ok(files) = &r {
            if self.addition.user_id == 0 {
                self.addition.user_id = AliyunShare115Addition::parse_uid(&self.addition.cookie);
                self.persist(db).await?;
            }
        }
        r
    }

    /// Resolve CDN links: copy share file → own drive → SHA1 → 115 rapid →
    /// downurl.
    pub async fn get_dlinks(
        &mut self,
        db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let user_id = self.addition.user_id;
        if user_id == 0 {
            return Err(AppError::CloudDrive(
                "无法从 115 cookie 解析 UID，请检查 cookie 是否完整（需 UID=…）".into(),
            ));
        }
        let mut open = self.open_driver();
        let mut share = self.share_driver();
        // ensure tokens first
        let _ = open.ensure_access_token(db).await?;
        let at = open.addition.access_token.clone();
        let _ = open.ensure_drive_id(db, &at).await?;
        let _ = share.ensure_access_token(db).await?;
        let _ = share.ensure_share_token(db).await?;
        self.sync_caches(&open, &share);

        let ua = crate::commands::cloud_driver::m115_ua().await;
        let cookie = self.addition.cookie.clone();
        let dir_id = self.addition.dir_id.trim().to_string();
        let user_key = self.addition.user_key.clone();
        let share_id = self.addition.share_id.clone();
        let share_drive = share.addition.drive_id.clone();
        let own_drive = open.addition.drive_id.clone();
        let copy_dir = {
            let d = self.addition.open_copy_dir.trim();
            if d.is_empty() {
                "root".to_string()
            } else {
                d.to_string()
            }
        };
        let access = open.addition.access_token.clone();
        let share_token = share.addition.share_token.clone();
        // The copy API lives on api.alipan.com (share domain) — it wants the
        // token issued by auth.alipan.com, not the open-platform token.
        let share_access = share.addition.access_token.clone();
        let client = Self::http_client(&ua)?;

        let mut result = Vec::new();
        for file in files {
            if file.isdir {
                continue;
            }
            if share_drive.is_empty() {
                return Err(AppError::CloudDrive(
                    "阿里分享 drive_id 为空，请先浏览一次分享目录".into(),
                ));
            }
            // 1) copy the share file into the own drive
            let copied = self
                .copy_to_own(
                    &client,
                    &share_access,
                    &share_token,
                    &share_id,
                    &share_drive,
                    &own_drive,
                    &copy_dir,
                    &file.id,
                    &file.name,
                )
                .await?;
            // 2) find the copied file in the own drive → SHA1
            let (copy_id, sha1) = self
                .find_copied(&mut open, db, &copy_dir, &file.name, file.size, copied)
                .await?;
            if sha1.is_empty() {
                return Err(AppError::CloudDrive(format!(
                    "{} 转存后未取到 SHA1",
                    file.name
                )));
            }
            // 3) 115 rapid upload
            let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
            let stamp = format!("{}", ts);
            let (base, ext) = match file.name.rsplit_once('.') {
                Some((b, e)) => (b.to_string(), e.to_string()),
                None => (file.name.clone(), String::new()),
            };
            let target_name = if ext.is_empty() {
                format!("{base}_{stamp}")
            } else {
                format!("{base}_{stamp}.{ext}")
            };
            let up = crate::commands::m115_rapid::rapid_upload(
                &cookie,
                &ua,
                user_id,
                &user_key,
                &dir_id,
                &target_name,
                file.size,
                &sha1,
            )
            .await?;
            let (dlink, ck) = self.downurl(&ua, &cookie, &up.pickcode, &file.name).await?;
            // The CDN rejects the download unless it carries the cookie issued
            // by this downurl response (same rule as the 115 driver).
            let mut hc = cookie.clone();
            for c in &ck {
                hc.push_str("; ");
                hc.push_str(c);
            }
            result.push((file.clone(), dlink, vec![
                format!("User-Agent: {ua}"),
                format!("Cookie: {hc}"),
            ]));
            let _ = copy_id;
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何文件的 115 下载地址".into()));
        }
        Ok(result)
    }

    /// `POST /v2/file/copy` with the share token; returns the server message.
    async fn copy_to_own(
        &self,
        client: &reqwest::Client,
        access: &str,
        share_token: &str,
        share_id: &str,
        share_drive: &str,
        own_drive: &str,
        to_parent: &str,
        file_id: &str,
        name: &str,
    ) -> Result<serde_json::Value, AppError> {
        let resp = client
            .post("https://api.alipan.com/v2/file/copy")
            .header("Authorization", format!("Bearer\t{access}"))
            .header("x-share-token", share_token)
            .header("X-Canary", "client=web,app=share,version=v2.3.1")
            .json(&serde_json::json!({
                "drive_id": share_drive,
                "file_id": file_id,
                "to_drive_id": own_drive,
                "to_parent_file_id": to_parent,
            }))
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("转存 {} 到云盘失败: {e}", name)))?;
        let status = resp.status();
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析转存响应失败: {e}")))?;
        if !status.is_success() {
            return Err(AppError::CloudDrive(format!(
                "转存 {} 失败（HTTP {status}）：{}",
                name,
                json.get("message").and_then(|m| m.as_str()).unwrap_or("")
            )));
        }
        Ok(json)
    }

    /// Poll the own-drive copy dir until a file matching name+size appears.
    async fn find_copied(
        &mut self,
        open: &mut AliyunOpenDriver,
        db: &Database,
        copy_dir: &str,
        name: &str,
        size: u64,
        _copy_resp: serde_json::Value,
    ) -> Result<(String, String), AppError> {
        // The copy is async; poll the target dir up to ~30s.
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let list = open.list(db, copy_dir).await?;
            for f in list {
                if f.name == name && f.size == size && !f.sha1.is_empty() {
                    return Ok((f.id.clone(), f.sha1.clone()));
                }
            }
        }
        Err(AppError::CloudDrive(format!(
            "分享文件 {name} 转存到云盘超时（30s 内未在目标目录找到）"
        )))
    }

    /// pickcode → CDN downlink (same flow as the 115 driver).
    /// Returns `(url, set_cookies)`; the caller must merge the cookies into
    /// the download header (the CDN URL is bound to them).
    async fn downurl(
        &self,
        ua: &str,
        cookie: &str,
        pickcode: &str,
        name: &str,
    ) -> Result<(String, Vec<String>), AppError> {
        let client = Self::http_client(ua)?;
        let key = m115_crypto::generate_key();
        let body = serde_json::json!({ "pickcode": pickcode }).to_string();
        let data = m115_crypto::encode(body.as_bytes(), &key);
        let url = format!("{M115_DOWN_URL}?t={}", chrono::Utc::now().timestamp());
        let resp = client
            .post(&url)
            .header("Cookie", cookie)
            .form(&[("data", data.as_str())])
            .header("Content-Type", "application/json;charset=UTF-8")
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("请求 115 下载地址失败: {e}")))?;
        let status = resp.status();
        let set_cookies: Vec<String> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::CloudDrive(format!("读取 115 下载响应失败: {e}")))?;
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| AppError::CloudDrive(format!("解析 115 下载响应失败: {e}")))?;
        let state = json["state"].as_bool().unwrap_or(false);
        if !status.is_success() || !state {
            return Err(AppError::CloudDrive(format!(
                "获取 115 下载地址失败（HTTP {status}）：{}",
                json["error"].as_str().unwrap_or("未知错误")
            )));
        }
        let encoded = json["data"]
            .as_str()
            .ok_or_else(|| AppError::CloudDrive("115 下载响应缺少 data".into()))?;
        let plain = m115_crypto::decode(encoded, &key)
            .map_err(|e| AppError::CloudDrive(format!("解密 115 下载响应失败: {e}")))?;
        let plain_json: serde_json::Value = serde_json::from_slice(&plain)
            .map_err(|e| AppError::CloudDrive(format!("解析 115 下载数据失败: {e}")))?;
        fn extract_dlink(v: &serde_json::Value) -> Option<&str> {
            v["url"]["url"].as_str().or_else(|| {
                v.as_object()
                    .and_then(|o| o.values().find_map(extract_dlink))
            })
        }
        extract_dlink(&plain_json)
            .or_else(|| {
                plain_json
                    .as_array()
                    .and_then(|a| a.iter().find_map(extract_dlink))
            })
            .map(|u| (u.to_string(), set_cookies))
            .ok_or_else(|| AppError::CloudDrive(format!("115 未返回 {name} 的下载地址")))
    }
}
