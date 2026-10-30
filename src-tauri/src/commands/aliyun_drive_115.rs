//! Aliyun Drive → 115 driver ("阿里云盘转115", alist-style ali2115).
//!
//! Browsing shows the user's Aliyun Drive (open platform) tree; downloading
//! goes through the 115 rapid upload (秒传) path, exactly like xiaoya's
//! ali2115: read the file's SHA1 (`content_hash`) → 115 `ApiUploadInit`
//! (QuickID = SHA1) → pickcode → existing `downurl` → CDN link. The rapid
//! upload hits only when the file already exists in the 115 library, costs no
//! cloud-download quota, and creates a file (with a timestamp suffix) in the
//! configured 115 dir.

use serde::{Deserialize, Serialize};

use crate::commands::aliyun_open::{
    default_drive_type, default_root, AliyunOpenAddition, AliyunOpenDriver,
};
use crate::commands::netdisk_http;
use crate::error::AppError;
use crate::commands::m115_crypto;
use crate::database::Database;

const M115_DOWN_URL: &str = "https://proapi.115.com/app/chrome/downurl";

fn default_115_dir() -> String {
    "0".into()
}

/// Combined addition: Aliyun open credentials + 115 session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliyunDrive115Addition {
    pub refresh_token: String,
    #[serde(default = "default_root")]
    pub root_folder_id: String,
    /// Alipan drive: default / resource / backup.
    #[serde(default = "default_drive_type")]
    pub drive_type: String,
    /// QR login channel (`messense` / `xiaoya`) recorded at scan time; the
    /// embedded open driver auto-selects the channel's refresh endpoint.
    #[serde(default)]
    pub qr_channel: String,
    /// 115 web-session cookie (UID/CID/SEID[/KID]).
    pub cookie: String,
    /// 115 QR device source recorded at scan time (same convention as the
    /// standalone 115 driver; empty = default alipaymini).
    #[serde(default)]
    pub qr_source: String,
    /// Cached 115 flash-transfer target cid. Learned once as the drive root
    /// (`0` → files land in 最近接收), persisted and reused afterwards.
    #[serde(default)]
    pub flash_dir_id: String,
    // ---- runtime caches (persisted) ----
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub token_expires_at: u64,
    #[serde(default)]
    pub drive_id: String,
    #[serde(default)]
    pub user_id: i64,
}

impl AliyunDrive115Addition {
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

pub struct AliyunDrive115Driver {
    pub id: i64,
    pub addition: AliyunDrive115Addition,
}

impl AliyunDrive115Driver {
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

    /// Build the embedded Aliyun open driver (shares the token cache).
    /// `no_persist`: never overwrite this storage's addition with the open
    /// driver's JSON (it would wipe cookie/user_key).
    fn open_driver(&self) -> AliyunOpenDriver {
        AliyunOpenDriver {
            id: self.id,
            addition: AliyunOpenAddition {
                refresh_token: self.addition.refresh_token.clone(),
                root_folder_id: self.addition.root_folder_id.clone(),
                drive_type: self.addition.drive_type.clone(),
                order_by: String::new(),
                order_direction: String::new(),
                qr_channel: self.addition.qr_channel.clone(),
                access_token: self.addition.access_token.clone(),
                token_expires_at: self.addition.token_expires_at,
                drive_id: self.addition.drive_id.clone(),
            },
            no_persist: true,
        }
    }

    fn sync_open_cache(&mut self, open: &AliyunOpenDriver) {
        self.addition.access_token = open.addition.access_token.clone();
        self.addition.token_expires_at = open.addition.token_expires_at;
        self.addition.drive_id = open.addition.drive_id.clone();
        self.addition.refresh_token = open.addition.refresh_token.clone();
    }

    /// List the Aliyun tree (open driver).
    pub async fn list(
        &mut self,
        db: &Database,
        file_id: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let mut open = self.open_driver();
        let r = open.list(db, file_id).await;
        let rotated = self.addition.refresh_token != open.addition.refresh_token
            || self.addition.access_token != open.addition.access_token;
        self.sync_open_cache(&open);
        if rotated {
            // no_persist 的 open 驱动不落库；旋转的 refresh_token 由这里保存。
            self.persist(db).await?;
        }
        if r.is_ok() {
            if self.addition.user_id == 0 {
                self.addition.user_id = AliyunDrive115Addition::parse_uid(&self.addition.cookie);
                self.persist(db).await?;
            }
        }
        r
    }

    /// Resolve CDN links: SHA1 → 115 rapid upload → downurl.
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
        let ua = crate::commands::cloud_driver::m115_ua().await;
        let cookie = self.addition.cookie.clone();
        // 闪传目标 cid 缓存：首次动态解析 115「最近接收」目录（无则自动
        // 创建）并落库，后续直接复用；与阿里分享转存目录（xiaoya）同一
        // 套解析+兜底逻辑。
        if self.addition.flash_dir_id.is_empty() {
            self.addition.flash_dir_id =
                crate::commands::m115_rapid::resolve_recent_recv_cid(&cookie, &ua).await?;
            self.persist(db).await?;
        }
        let dir_id = self.addition.flash_dir_id.clone();
        // user_key 必须从 115 uploadinfo 动态获取：它参与闪传签名（sig），
        // 签名回合第二轮会用 userkey 校验，传空串会被 115 以 status=4 拒绝，
        // 导致闪传从未真正命中（只能靠目录 find 兜底捡现成）。
        let user_key = crate::commands::m115_rapid::fetch_upload_userkey(&cookie, &ua).await?;
        // Refresh the embedded open-driver token once up front (rotates and
        // persists); the sign-proof reader below uses it for Range downloads.
        let mut open = self.open_driver();
        let access_token = open.ensure_access_token(db).await?;
        self.sync_open_cache(&open);
        // no_persist 的 open 驱动不落库；旋转的 refresh_token 由这里用完整
        // addition（含 115 cookie）保存。
        self.persist(db).await?;
        let mut result = Vec::new();
        for file in files {
            if file.isdir || file.sha1.is_empty() {
                if file.isdir {
                    continue;
                }
                return Err(AppError::CloudDrive(format!(
                    "{} 缺少 SHA1，无法转存 115（请确认该文件在阿里云盘中可读）",
                    file.name
                )));
            }
            // 115 目标目录文件名直接用原名：配合闪传前查重，同一文件
            // 多次下载只保留首份，不会重复堆积带时间戳的副本。
            let target_name = file.name.clone();
            // 闪传 + 签名证明 + status=4 回退：命中（7）用阿里 Range 读区间，
            // 被拒（4）时按 SHA1 在目标目录里直接找 pickcode。
            let reader = |range: &str| {
                let range = range.to_string();
                let token = access_token.clone();
                let drive_id = file.drive_id.clone();
                let file_id = file.id.clone();
                async move {
                        let client = reqwest::Client::new();
                        let resp = client
                            .post("https://openapi.aliyundrive.com/adrive/v1.0/openFile/getDownloadUrl")
                            .bearer_auth(&token)
                            .json(&serde_json::json!({"drive_id": drive_id, "file_id": file_id}))
                            .send()
                            .await
                            .map_err(|e| AppError::CloudDrive(format!("获取阿里下载地址失败: {e}")))?;
                        let j: serde_json::Value = netdisk_http::read_json(resp, "aliyun-115")
                            .await
                            .map_err(|e| AppError::CloudDrive(format!("解析阿里下载地址失败: {e}")))?;
                        let url = j["url"].as_str().ok_or_else(|| {
                            AppError::CloudDrive("阿里下载地址为空（文件可能已失效）".into())
                        })?;
                        let range_resp = client
                            .get(url)
                            .header("Range", format!("bytes={range}"))
                            .send()
                            .await
                            .map_err(|e| AppError::CloudDrive(format!("下载阿里文件校验区间失败: {e}")))?;
                        let bytes = netdisk_http::read_bytes(range_resp, "aliyun-115:range-check")
                            .await
                            .map_err(|e| AppError::CloudDrive(format!("读取阿里文件校验区间失败: {e}")))?;
                        Ok(bytes.to_vec())
                    }
            };
            // 0) 先去 115 闪存目录按 SHA1 查重：命中 → 直接复用其 pickcode
            //    下载（省一次闪传+签名回合，也避免重复闪传覆盖同名文件）。
            if let Some(pc) = crate::commands::m115_rapid::find_pickcode_by_sha1(
                &cookie, &ua, &dir_id, &file.sha1,
            )
            .await?
            {
                let (dlink, ck) = self.downurl(&ua, &cookie, &pc, &file.name).await?;
                // The CDN rejects the download unless it carries the cookie
                // issued by this downurl response (same rule as the 115 driver).
                let mut hc = cookie.clone();
                for c in &ck {
                    hc.push_str("; ");
                    hc.push_str(c);
                }
                result.push((file.clone(), dlink, vec![
                    format!("User-Agent: {ua}"),
                    format!("Cookie: {hc}"),
                ]));
                continue;
            }
            // 兜底：缓存 cid 失效（目录被删/账号不同）→ 重新解析最近接收
            // 目录（无则创建），更新缓存后重试一次。
            let mut pickcode = crate::commands::m115_rapid::rapid_or_find_pickcode(
                &cookie, &ua, user_id, &user_key, &dir_id, &target_name, file.size,
                &file.sha1, &reader,
            )
            .await;
            if pickcode.is_err() {
                let new_cid =
                    crate::commands::m115_rapid::resolve_recent_recv_cid(&cookie, &ua).await?;
                if new_cid != self.addition.flash_dir_id {
                    self.addition.flash_dir_id = new_cid.clone();
                    self.persist(db).await?;
                }
                pickcode = crate::commands::m115_rapid::rapid_or_find_pickcode(
                    &cookie, &ua, user_id, &user_key, &new_cid, &target_name, file.size,
                    &file.sha1, &reader,
                )
                .await;
            }
            let pickcode = pickcode?;
            let (dlink, ck) = self.downurl(&ua, &cookie, &pickcode, &file.name).await?;
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
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何文件的 115 下载地址".into()));
        }
        Ok(result)
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
        netdisk_http::log_meta(&resp, "aliyun-115:downurl");
        let status = resp.status();
        let set_cookies: Vec<String> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_string))
            .filter(|s| !s.is_empty())
            .collect();
        let text = netdisk_http::read_text(resp, "aliyun-115")
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
