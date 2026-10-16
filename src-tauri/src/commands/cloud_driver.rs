//! Cloud drive driver abstraction (alist-style).
//!
//! Each mounted storage row (`cloud_storages`) is materialized into a driver
//! instance dispatching on its `driver` field. Currently only the Baidu
//! Netdisk driver exists; new drivers (quark/115/…) add a variant here plus a
//! match arm in `CloudDriver::from_storage`.
//!
//! Driver configuration lives in the storage `addition` JSON; the access_token
//! is persisted there too (per user decision), so token renewal rewrites the
//! storage row instead of a separate credential file.

use crate::database::{CloudStorage, Database, DatabaseState};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_store::StoreExt;

pub const BAIDU_DEFAULT_CLIENT_ID: &str = "hq9yQ9w9kR4YHj1kyYafLygVocobh7Sf";
pub const BAIDU_DEFAULT_CLIENT_SECRET: &str = "YH2VpZcFJHYNnV6vLfHQXDBhcE7ZChyE";
const OAUTH_TOKEN_URL: &str = "https://openapi.baidu.com/oauth/2.0/token";
const BAIDU_FILE_LIST_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/file?method=list&showempty=1";
const BAIDU_FILE_METAS_URL: &str = "https://pan.baidu.com/rest/2.0/xpan/multimedia";
const USER_AGENT: &str = "pan.baidu.com";
const PAN_REFERER: &str = "https://pan.baidu.com/";

/// Quark drive API (mirrors alist `drivers/quark_uc`).
const QUARK_API: &str = "https://drive.quark.cn/1/clouddrive";
const QUARK_REFERER: &str = "https://pan.quark.cn";
const QUARK_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) quark-cloud-drive/2.5.20 Chrome/100.0.4896.160 Electron/18.3.5.4-b478491100 Safari/537.36 Channel/pckk_other_ch";
/// Session cookie reissued by the server; expires in ~3h.
const QUARK_PUUS_COOKIE: &str = "__puus";
/// Max attempts per API call (1 original + 1 after a session-cookie refresh).
const QUARK_MAX_ATTEMPTS: usize = 2;

/// Baidu Netdisk driver addition (persisted in `cloud_storages.addition`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaiduAddition {
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    /// Unix seconds when access_token expires (0 = unknown → treat as expired).
    #[serde(default)]
    pub token_expires_at: u64,
    /// Mount root inside the drive; `/` for the whole drive.
    #[serde(default = "default_root_path")]
    pub root_path: String,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    #[serde(default = "default_client_secret")]
    pub client_secret: String,
}

fn default_root_path() -> String {
    "/".into()
}
fn default_client_id() -> String {
    BAIDU_DEFAULT_CLIENT_ID.into()
}
fn default_client_secret() -> String {
    BAIDU_DEFAULT_CLIENT_SECRET.into()
}

impl BaiduAddition {
    pub fn parse(json: &str) -> Result<Self, AppError> {
        serde_json::from_str(json)
            .map_err(|e| AppError::CloudDrive(format!("解析驱动配置失败: {e}")))
    }
    fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|e| AppError::CloudDrive(format!("序列化驱动配置失败: {e}")))
    }
    pub fn normalize_root(&self) -> String {
        let root = self.root_path.trim();
        if root.is_empty() {
            return "/".into();
        }
        root.to_string()
    }
}

/// Quark Netdisk driver addition (persisted in `cloud_storages.addition`).
/// Quark has no public OAuth; authentication is the web-session cookie copied
/// from pan.quark.cn, matching alist's quark driver. The `__puus` session
/// cookie is refreshed on demand by the driver and persisted back here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarkAddition {
    pub cookie: String,
    /// Browse root; `0` is the drive root, a sub-directory fid works too.
    #[serde(default = "default_quark_root")]
    pub root_id: String,
}

fn default_quark_root() -> String {
    "0".into()
}

impl QuarkAddition {
    pub fn parse(json: &str) -> Result<Self, AppError> {
        serde_json::from_str(json)
            .map_err(|e| AppError::CloudDrive(format!("解析驱动配置失败: {e}")))
    }
    fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|e| AppError::CloudDrive(format!("序列化驱动配置失败: {e}")))
    }
}

/// Best-effort HTML unescape for names returned by Quark.
fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// A materialized driver instance (enum dispatch keeps this dependency-free).
pub enum CloudDriver {
    Baidu(BaiduDriver),
    Quark(QuarkDriver),
}

impl CloudDriver {
    pub fn from_storage(storage: &CloudStorage) -> Result<Self, AppError> {
        match storage.driver.as_str() {
            "baidu_netdisk" => Ok(CloudDriver::Baidu(BaiduDriver {
                id: storage.id,
                addition: BaiduAddition::parse(&storage.addition)?,
            })),
            "quark" => Ok(CloudDriver::Quark(QuarkDriver {
                id: storage.id,
                addition: QuarkAddition::parse(&storage.addition)?,
            })),
            other => Err(AppError::CloudDrive(format!(
                "不支持的驱动类型：{other}"
            ))),
        }
    }

    pub fn driver_name(&self) -> &'static str {
        match self {
            CloudDriver::Baidu(_) => "baidu_netdisk",
            CloudDriver::Quark(_) => "quark",
        }
    }

    /// List the children of `path`. For Baidu this is an absolute drive path
    /// (including the mount root); for Quark it is a directory fid (`0` = root).
    pub async fn list(
        &mut self,
        db: &Database,
        path: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let result = match self {
            CloudDriver::Baidu(d) => {
                let token = d.ensure_access_token(db).await?;
                d.list(path, &token).await
            }
            CloudDriver::Quark(d) => d.list(db, path).await,
        };
        let status = match &result {
            Ok(_) => "正常".to_string(),
            Err(e) => format!("错误: {e}"),
        };
        match self {
            CloudDriver::Baidu(d) => {
                d.set_status(db, &status).await;
            }
            CloudDriver::Quark(d) => {
                d.set_status(db, &status).await;
            }
        }
        result
    }
}

/// Baidu Netdisk driver: open-API list + dlink download with the same UA/Referer
/// conventions proven against Baidu (dlink must carry access_token or it 403s).
pub struct BaiduDriver {
    id: i64,
    addition: BaiduAddition,
}

impl BaiduDriver {
    fn http_client() -> Result<reqwest::Client, AppError> {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
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

    async fn persist(&self, db: &Database) -> Result<(), AppError> {
        db.update_storage_addition(self.id, &self.addition.to_json()?)
            .await?;
        Ok(())
    }

    async fn set_status(&self, db: &Database, status: &str) -> Result<(), AppError> {
        db.set_storage_status(self.id, status).await
    }

    /// Cached access_token if valid, else renew via refresh_token and persist.
    async fn ensure_access_token(&mut self, db: &Database) -> Result<String, AppError> {
        if self.addition.token_expires_at > Self::now() && !self.addition.access_token.is_empty() {
            return Ok(self.addition.access_token.clone());
        }
        if self.addition.refresh_token.is_empty() {
            return Err(AppError::CloudDrive(
                "缺少 refresh_token，请在存储管理中编辑该存储填入 refresh_token".into(),
            ));
        }
        let client = Self::http_client()?;
        let resp = client
            .post(OAUTH_TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.addition.refresh_token.as_str()),
                ("client_id", self.addition.client_id.as_str()),
                ("client_secret", self.addition.client_secret.as_str()),
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

    async fn list(
        &mut self,
        path: &str,
        token: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let client = Self::http_client()?;
        // 百度 xpan list 不返回总数，参考 alist 的做法：start 递增翻页直到
        // 某页返回空列表，合并全量返回。每页 limit=1000（API 上限）。
        const LIMIT: usize = 1000;
        const MAX_PAGES: usize = 100; // 安全上限：最多 10 万文件，防异常死循环
        let mut out = Vec::new();
        let mut start = 0usize;
        for page in 0..MAX_PAGES {
            let url = format!(
                "{BAIDU_FILE_LIST_URL}&dir={}&limit={LIMIT}&start={start}&web=1&access_token={}",
                urlencoding::encode(path),
                token
            );
            let resp = client
                .get(&url)
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
                #[serde(rename = "server_mtime", default)]
                mtime: u64,
                #[serde(default)]
                category: u32,
            }

            let raw: Raw = serde_json::from_str(&text)
                .map_err(|e| AppError::CloudDrive(format!("解析文件列表失败: {e}")))?;
            match raw.errno.unwrap_or(0) {
                0 => {}
                9019 => {
                    return Err(AppError::CloudDrive(
                        "访问令牌已过期（errno=9019），请重新获取".into(),
                    ));
                }
                112 => {
                    return Err(AppError::CloudDrive("网盘页面已过期，请刷新后重试".into()));
                }
                other => {
                    return Err(AppError::CloudDrive(format!("百度返回错误 errno={other}")));
                }
            }
            let files = raw.list.unwrap_or_default();
            if files.is_empty() {
                break; // 已到最后一页
            }
            for f in files {
                out.push(crate::commands::CloudDriveFile {
                    fs_id: f.fs_id,
                    name: f.name,
                    size: f.size,
                    path: f.path,
                    isdir: f.isdir != 0,
                    mtime: f.mtime,
                    category: f.category,
                    id: f.fs_id.to_string(),
                    rel_path: String::new(),
                });
            }
            if out.len() < (page + 1) * LIMIT {
                break; // 不足一页 = 已是最后一页，省一次请求
            }
            start += LIMIT;
        }
        Ok(out)
    }

    /// Fetch download links for files; dlink carries the access_token (403 guard).
    /// Returns (file, url, aria2 headers) so the caller submits exactly what
    /// each driver needs (UA/Referer; Quark additionally requires the cookie).
    pub async fn get_dlinks(
        &mut self,
        db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let token = self.ensure_access_token(db).await?;
        let client = Self::http_client()?;
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
                .header("Referer", PAN_REFERER)
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求下载地址失败: {e}")))?;
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析下载地址失败: {e}")))?;
            match json["errno"].as_i64().unwrap_or(-1) {
                0 => {}
                9019 => {
                    return Err(AppError::CloudDrive(
                        "访问令牌已过期（errno=9019），请重新获取".into(),
                    ));
                }
                112 => {
                    return Err(AppError::CloudDrive(
                        "网盘页面已过期，请刷新后重试".into(),
                    ));
                }
                other => {
                    let msg = json["errmsg"].as_str().unwrap_or("未知错误");
                    return Err(AppError::CloudDrive(format!(
                        "获取下载地址失败（errno={other}）：{msg}"
                    )));
                }
            }
            if let Some(list) = json["list"].as_array() {
                for item in list {
                    let Some(fs_id) = item["fs_id"].as_u64() else { continue };
                    let Some(dlink) = item["dlink"].as_str() else { continue };
                    if let Some(file) = files.iter().find(|f| f.fs_id == fs_id) {
                        let dlink = if dlink.contains("access_token=") {
                            dlink.to_string()
                        } else {
                            format!("{dlink}&access_token={token}")
                        };
                        result.push((
                            file.clone(),
                            dlink,
                            vec![
                                format!("User-Agent: {USER_AGENT}"),
                                format!("Referer: {PAN_REFERER}"),
                            ],
                        ));
                    }
                }
            }
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何文件的下载地址".into()));
        }
        Ok(result)
    }
}

// ============================== Quark driver ==============================

/// Quark Netdisk driver: web-session cookie auth + fid-based list and download
/// (mirrors alist `drivers/quark_uc`). The `__puus` session cookie is refreshed
/// on demand: a failed call strips it and hits /config so the server reissues a
/// fresh one, then persists the updated cookie to the storage row.
pub struct QuarkDriver {
    id: i64,
    addition: QuarkAddition,
}

impl QuarkDriver {
    fn http_client() -> Result<reqwest::Client, AppError> {
        reqwest::Client::builder()
            .user_agent(QUARK_UA)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
    }

    async fn persist(&self, db: &Database) -> Result<(), AppError> {
        db.update_storage_addition(self.id, &self.addition.to_json()?)
            .await?;
        Ok(())
    }

    async fn set_status(&self, db: &Database, status: &str) -> Result<(), AppError> {
        db.set_storage_status(self.id, status).await
    }

    /// Cookie without the `__puus` session value.
    fn strip_puus(cookie: &str) -> String {
        cookie
            .split(';')
            .map(|p| p.trim())
            .filter(|p| !p.starts_with(&format!("{QUARK_PUUS_COOKIE}=")))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn merge_puus(cookie: &str, new_puus: &str) -> String {
        let stripped = Self::strip_puus(cookie);
        if stripped.is_empty() {
            format!("{QUARK_PUUS_COOKIE}={new_puus}")
        } else {
            format!("{stripped}; {QUARK_PUUS_COOKIE}={new_puus}")
        }
    }

    /// Ask the server to reissue the session cookie: GET /config without
    /// `__puus`. Persist the updated cookie and return it.
    async fn refresh_session(&mut self, db: &Database) -> Result<(), AppError> {
        let client = Self::http_client()?;
        let stripped = Self::strip_puus(&self.addition.cookie);
        let url = format!("{QUARK_API}/config?pr=ucpro&fr=pc");
        let resp = client
            .get(&url)
            .header("Cookie", stripped)
            .header("Referer", QUARK_REFERER)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("刷新夸克会话失败: {e}")))?;
        let new_puus = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|s| {
                let pair = s.split(';').next().unwrap_or("");
                let mut it = pair.splitn(2, '=');
                match (it.next(), it.next()) {
                    (Some(k), Some(v)) if k.trim() == QUARK_PUUS_COOKIE => Some(v.to_string()),
                    _ => None,
                }
            });
        match new_puus {
            Some(p) => {
                self.addition.cookie = Self::merge_puus(&self.addition.cookie, &p);
                self.persist(db).await?;
                Ok(())
            }
            None => Err(AppError::CloudDrive(
                "夸克未下发新的会话 cookie，可能已完全失效，请重新获取 cookie".into(),
            )),
        }
    }

    /// Whether the provider envelope or HTTP status indicates the session
    /// cookie is stale and worth one refresh+retry.
    fn cookie_stale(status: reqwest::StatusCode, code: i64, message: &str) -> bool {
        status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::UNAUTHORIZED
            || code != 0 && (message.contains("登录") || message.contains("token") || message.contains("cookie") || message.contains("登录状态"))
    }

    async fn list(
        &mut self,
        db: &Database,
        fid: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        const SIZE: usize = 100;
        const MAX_PAGES: usize = 100; // 安全上限：最多 1 万文件，防异常死循环
        let mut out = Vec::new();
        let mut page = 1usize;
        let mut total_hint: u64 = 0;
        loop {
            let mut page_ok = false;
            for attempt in 0..QUARK_MAX_ATTEMPTS {
                let client = Self::http_client()?;
                let url = format!(
                    "{QUARK_API}/file/sort?pdir_fid={}&_size={SIZE}&_fetch_total=1&fetch_all_file=1&fetch_risk_file_name=1&_page={page}&pr=ucpro&fr=pc",
                    urlencoding::encode(fid)
                );
                let resp = client
                    .get(&url)
                    .header("Cookie", &self.addition.cookie)
                    .header("Referer", QUARK_REFERER)
                    .send()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("请求夸克文件列表失败: {e}")))?;
                let status = resp.status();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("读取夸克列表响应失败: {e}")))?;
                let json: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| AppError::CloudDrive(format!("解析夸克列表失败: {e}")))?;
                let code = json["code"].as_i64().unwrap_or(-1);
                let message = json["message"].as_str().unwrap_or("");
                if !status.is_success() || code != 0 {
                    if attempt + 1 < QUARK_MAX_ATTEMPTS && Self::cookie_stale(status, code, message)
                    {
                        if self.refresh_session(db).await.is_ok() {
                            continue; // retry the same page with the fresh cookie
                        }
                    }
                    return Err(AppError::CloudDrive(format!(
                        "夸克返回错误 code={code}: {}",
                        if message.is_empty() { "未知错误" } else { message }
                    )));
                }
                let list = json["data"]["list"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut skipped = 0usize;
                for item in &list {
                    // Quark returns `fid` as a JSON string in some API versions
                    // and as a plain number in others; accept both.
                    let fid_str = item["fid"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| item["fid"].as_u64().map(|n| n.to_string()))
                        .or_else(|| item["fid"].as_i64().map(|n| n.to_string()));
                    let Some(fid_str) = fid_str else {
                        skipped += 1;
                        continue;
                    };
                    // Quark fids are 32-char hex strings (do not fit u64), so
                    // fs_id is 0 and the universal `id` carries the fid.
                    let is_file = item["file"]
                        .as_bool()
                        .unwrap_or_else(|| item["file"].as_u64().map(|v| v != 0).unwrap_or(false));
                    out.push(crate::commands::CloudDriveFile {
                        fs_id: 0,
                        name: html_unescape(item["file_name"].as_str().unwrap_or("")),
                        size: item["size"].as_u64().unwrap_or(0),
                        path: fid_str.clone(),
                        isdir: !is_file,
                        mtime: (item["updated_at"].as_i64().unwrap_or(0).max(0) / 1000) as u64,
                        category: item["category"].as_u64().unwrap_or(0) as u32,
                        id: fid_str.clone(),
                        rel_path: String::new(),
                    });
                }
                if out.is_empty() && skipped > 0 {
                    log::warn!(
                        "quark: skipped {skipped}/{} items for fid {fid}; sample={}",
                        list.len(),
                        serde_json::to_string(&list[0]).unwrap_or_default()
                    );
                }
                let total = json["metadata"]["_total"].as_u64().unwrap_or(0);
                if total > 0 {
                    total_hint = total;
                }
                page_ok = true;
                break;
            }
            if !page_ok {
                // Both attempts exhausted without success; error already returned.
                break;
            }
            // Next page? Quark reports the total in `metadata._total`; keep
            // fetching until the current page covers it.
            if page as u64 * SIZE as u64 >= total_hint || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch download URLs: POST /file/download with the fids; the returned
    /// URL is signed against the cookie used for the request, so the same
    /// cookie snapshot must ride along in the aria2 headers.
    pub async fn get_dlinks(
        &mut self,
        db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let mut result = Vec::new();
        for chunk in files.chunks(100) {
            for attempt in 0..QUARK_MAX_ATTEMPTS {
                let client = Self::http_client()?;
                let fids: Vec<String> = chunk.iter().map(|f| f.id.clone()).collect();
                let body = serde_json::json!({ "fids": fids });
                let url = format!("{QUARK_API}/file/download?pr=ucpro&fr=pc");
                let req_cookie = self.addition.cookie.clone();
                let resp = client
                    .post(&url)
                    .header("Cookie", &req_cookie)
                    .header("Referer", QUARK_REFERER)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("请求夸克下载地址失败: {e}")))?;
                let status = resp.status();
                let json: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("解析夸克下载地址失败: {e}")))?;
                let code = json["code"].as_i64().unwrap_or(-1);
                let message = json["message"].as_str().unwrap_or("");
                if !status.is_success() || code != 0 {
                    if attempt + 1 < QUARK_MAX_ATTEMPTS && Self::cookie_stale(status, code, message)
                    {
                        if self.refresh_session(db).await.is_ok() {
                            continue;
                        }
                    }
                    return Err(AppError::CloudDrive(format!(
                        "获取夸克下载地址失败（code={code}）：{}",
                        if message.is_empty() { "未知错误" } else { message }
                    )));
                }
                let data = json["data"].as_array().cloned().unwrap_or_default();
                let headers = vec![
                    format!("User-Agent: {QUARK_UA}"),
                    format!("Referer: {QUARK_REFERER}"),
                    format!("Cookie: {req_cookie}"),
                ];
                for (i, item) in data.iter().enumerate() {
                    let Some(url) = item["download_url"].as_str() else { continue };
                    let Some(file) = chunk.get(i) else { continue };
                    result.push((file.clone(), url.to_string(), headers.clone()));
                }
                break;
            }
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何夸克文件的下载地址".into()));
        }
        Ok(result)
    }
}

// ============================== storage-scoped commands ==============================

use crate::commands::cloud_drive::{resolve_download_dir, CloudDriveFile, CloudDriveResult};
use crate::services::downloads::{submit, TaskInput};

/// Recursively expand checked folders into contained files via the driver.
/// Populates each file's `rel_path` with the dir chain rooted at the checked
/// (level-1) item: a checked folder `A` becomes `A`, its sub-folder `B` becomes
/// `A/B`, files land in their parent chain; level-1 files keep an empty chain.
async fn expand_folders(
    driver: &mut CloudDriver,
    db: &Database,
    files: Vec<CloudDriveFile>,
) -> Result<Vec<CloudDriveFile>, AppError> {
    let mut result: Vec<CloudDriveFile> = Vec::new();
    let mut queue: Vec<CloudDriveFile> = files;
    for f in queue.iter_mut() {
        f.rel_path = if f.isdir { f.name.clone() } else { String::new() };
    }
    let mut requests = 0usize;
    while let Some(item) = queue.pop() {
        if !item.isdir {
            result.push(item);
            continue;
        }
        let children = driver.list(db, &item.path).await?;
        requests += 1;
        // Pace requests like LinkSwift: rest every 50 list calls.
        if requests % 50 == 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        for mut child in children {
            child.rel_path = if child.isdir {
                if item.rel_path.is_empty() {
                    child.name.clone()
                } else {
                    format!("{}/{}", item.rel_path, child.name)
                }
            } else {
                item.rel_path.clone()
            };
            queue.push(child);
        }
    }
    Ok(result)
}

/// Load a storage row and materialize its driver (disabled storages refuse).
async fn load_driver(
    db: &Database,
    storage_id: i64,
) -> Result<CloudDriver, AppError> {
    let storage = db
        .get_storage(storage_id)
        .await?
        .ok_or_else(|| AppError::CloudDrive("存储不存在".into()))?;
    if storage.disabled {
        return Err(AppError::CloudDrive("该存储已被禁用，请在存储管理中启用".into()));
    }
    CloudDriver::from_storage(&storage)
}

/// List a folder inside a storage (routes to the storage's driver).
#[tauri::command]
pub async fn cloud_drive_list_path(
    db: State<'_, DatabaseState>,
    storage_id: i64,
    path: String,
) -> Result<Vec<CloudDriveFile>, AppError> {
    let mut driver = load_driver(&db.0, storage_id).await?;
    driver.list(&db.0, &path).await
}

/// Download checked files from a storage: driver dlinks -> aria2.
#[tauri::command]
pub async fn cloud_drive_storage_download(
    app: AppHandle,
    db: State<'_, DatabaseState>,
    storage_id: i64,
    files: Vec<CloudDriveFile>,
    current_dir: Option<String>,
) -> Result<CloudDriveResult, AppError> {
    // Kept for API compatibility; download dirs come from each file's rel_path.
    let _ = &current_dir;
    if files.is_empty() {
        return Err(AppError::CloudDrive("没有选择文件".into()));
    }
    let mut driver = load_driver(&db.0, storage_id).await?;
    let files = expand_folders(&mut driver, &db.0, files).await?;
    if files.is_empty() {
        return Err(AppError::CloudDrive("所选文件夹中没有可下载的文件".into()));
    }
    let links = match &mut driver {
        CloudDriver::Baidu(d) => d.get_dlinks(&db.0, &files).await?,
        CloudDriver::Quark(d) => d.get_dlinks(&db.0, &files).await?,
    };

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
    for (file, dlink, headers) in &links {
        let mut options = serde_json::json!({
            "header": headers,
            "out": file.name,
        });
        if let Some(dir) = default_dir
            .as_deref()
            .and_then(|base| resolve_download_dir(base, &file.rel_path))
        {
            options["dir"] = serde_json::Value::String(dir);
        }
        match submit(&app, TaskInput::Uris(vec![dlink.clone()]), options, None).await {
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

