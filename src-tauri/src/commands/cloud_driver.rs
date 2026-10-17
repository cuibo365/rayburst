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

/// 115 Netdisk drive (mirrors alist `drivers/115` + SheltonZhu/115driver).
const M115_FILE_LIST_URL: &str = "https://webapi.115.com/files";
const M115_DOWN_URL: &str = "https://proapi.115.com/app/chrome/downurl";
const M115_APP_VERSION_URL: &str = "https://appversion.115.com/1/web/1.0/api/chrome";
/// Fallback 115Browser version, refreshed from appversion.115.com at runtime
/// (alist `appver.go`): the CDN rejects stale client versions with 403.
const M115_UA_FALLBACK: &str = "27.0.5.7";
/// Referer for list / QR endpoints; the download path intentionally carries
/// none (mirrors alist/SDK: downurl + CDN only see UA and Cookie).
const M115_REFERER: &str = "https://115.com/";
/// Default list page size (alist default 1000; server caps at MaxDirPageLimit).
const M115_PAGE_SIZE: i64 = 1000;
/// 115 QR login endpoints (SDK qrcode.go).
const M115_QRCODE_TOKEN_URL: &str = "https://qrcodeapi.115.com/api/1.0/web/1.0/token";
const M115_QRCODE_STATUS_URL: &str = "https://qrcodeapi.115.com/get/status/";
const M115_QRCODE_LOGIN_URL: &str = "https://passportapi.115.com/app/1.0/web/1.0/login/qrcode";

/// Latest 115Browser version (fetched once; falls back to `M115_UA_FALLBACK`).
static M115_APP_VER: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

/// `Mozilla/5.0 115Browser/<ver>` — the UA used for list/download requests.
/// The CDN URL returned by downurl is bound to this UA, so the same UA must
/// ride along in the aria2 headers (alist passes it through identically).
async fn m115_ua() -> String {
    let ver = M115_APP_VER
        .get_or_init(|| async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build();
            let ver = match client {
                Ok(c) => {
                    let json = match c.get(M115_APP_VERSION_URL).send().await {
                        Ok(resp) => resp.json::<serde_json::Value>().await.ok(),
                        Err(_) => None,
                    };
                    // SDK Versions is `data: { win: {version_code, ...}, android: {...} }`
                    json.and_then(|j| j["data"]["win"]["version_code"].as_str().map(str::to_string))
                }
                Err(_) => None,
            };
            ver.unwrap_or_else(|| M115_UA_FALLBACK.to_string())
        })
        .await;
    format!("Mozilla/5.0 115Browser/{ver}")
}

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

/// 115 Netdisk driver addition (persisted in `cloud_storages.addition`).
/// Auth is the web-session cookie copied from 115.com (UID/CID/SEID[/KID]),
/// matching alist's 115 driver. The cookie is stable for a long time; the
/// QR-code flow just fills it in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M115Addition {
    pub cookie: String,
    /// Browse root category id; `0` is the drive root.
    #[serde(default = "default_m115_root")]
    pub root_id: String,
}

fn default_m115_root() -> String {
    "0".into()
}

impl M115Addition {
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
    QuarkShare(QuarkShareDriver),
    M115(M115Driver),
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
            "quark_share" => Ok(CloudDriver::QuarkShare(QuarkShareDriver {
                id: storage.id,
                addition: QuarkShareAddition::parse(&storage.addition)?,
            })),
            "m115" => Ok(CloudDriver::M115(M115Driver {
                id: storage.id,
                addition: M115Addition::parse(&storage.addition)?,
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
            CloudDriver::QuarkShare(_) => "quark_share",
            CloudDriver::M115(_) => "m115",
        }
    }

    /// List the children of `path`. For Baidu this is an absolute drive path
    /// (including the mount root); for Quark/QuarkShare/M115 it is a directory
    /// id (`0` = root).
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
            CloudDriver::QuarkShare(d) => d.list(Some(db), path).await,
            CloudDriver::M115(d) => d.list(db, path).await,
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
            CloudDriver::QuarkShare(d) => {
                d.set_status(Some(db), &status).await;
            }
            CloudDriver::M115(d) => {
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
                    pdir_fid: String::new(),
                    share_fid_token: String::new(),
                    pickcode: String::new(),
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
                        pdir_fid: String::new(),
                        share_fid_token: String::new(),
                        pickcode: String::new(),
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

// ============================== 115 driver ==============================

/// 115 Netdisk driver: cookie-auth list + m115-encrypted download URLs.
/// The CDN URL is bound to the account cookie and the 115Browser UA, so both
/// must ride along in the aria2 headers (mirrors alist's Link() cookie fix).
pub struct M115Driver {
    id: i64,
    addition: M115Addition,
}

impl M115Driver {
    fn http_client(ua: &str) -> Result<reqwest::Client, AppError> {
        reqwest::Client::builder()
            .user_agent(ua)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
    }

    async fn set_status(&self, db: &Database, status: &str) -> Result<(), AppError> {
        db.set_storage_status(self.id, status).await
    }

    /// Normalize the browse root: empty → `0`.
    fn root(&self) -> String {
        let root = self.addition.root_id.trim();
        if root.is_empty() {
            "0".into()
        } else {
            root.to_string()
        }
    }

    /// Parse 115's update-time field: dirs send unix seconds, files send a
    /// `2006-01-02 15:04[:SS]` string in Beijing time.
    fn parse_mtime(t: &str) -> u64 {
        if let Ok(secs) = t.parse::<i64>() {
            return secs.max(0) as u64;
        }
        for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, fmt) {
                if let Some(offset) = chrono::FixedOffset::east_opt(8 * 3600) {
                    if let Some(dt) = dt.and_local_timezone(offset).single() {
                        return dt.timestamp().max(0) as u64;
                    }
                }
            }
        }
        0
    }

    async fn list(
        &mut self,
        _db: &Database,
        cid: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let dir = if cid.is_empty() { self.root() } else { cid.to_string() };
        const MAX_PAGES: usize = 100; // 安全上限，防异常死循环
        let mut out = Vec::new();
        let mut offset: i64 = 0;
        let ua = m115_ua().await;
        for _ in 0..MAX_PAGES {
            let client = Self::http_client(&ua)?;
            let offset_s = offset.to_string();
            let limit_s = M115_PAGE_SIZE.to_string();
            let url = reqwest::Url::parse_with_params(
                M115_FILE_LIST_URL,
                &[
                    ("aid", "1"),
                    ("cid", dir.as_str()),
                    ("o", "time"),
                    ("asc", "1"),
                    ("offset", offset_s.as_str()),
                    ("show_dir", "1"),
                    ("limit", limit_s.as_str()),
                    ("snap", "0"),
                    ("natsort", "0"),
                    ("record_open_time", "1"),
                    ("format", "json"),
                    ("fc_mix", "0"),
                ],
            )
            .map_err(|e| AppError::CloudDrive(format!("构造 115 列表地址失败: {e}")))?;
            let resp = client
                .get(url)
                .header("Cookie", &self.addition.cookie)
                .header("Referer", M115_REFERER)
                .header("Content-Type", "application/json;charset=UTF-8")
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 115 文件列表失败: {e}")))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| AppError::CloudDrive(format!("读取 115 列表响应失败: {e}")))?;
            let json: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| AppError::CloudDrive(format!("解析 115 列表失败: {e}")))?;
            let state = json["state"].as_bool().unwrap_or(false);
            if !status.is_success() || !state {
                let msg = json["error"]
                    .as_str()
                    .or_else(|| json["message"].as_str())
                    .unwrap_or("未知错误");
                if status == reqwest::StatusCode::FORBIDDEN
                    || status == reqwest::StatusCode::UNAUTHORIZED
                {
                    return Err(AppError::CloudDrive(format!(
                        "115 登录状态已失效（HTTP {status}），请重新获取 cookie：{msg}"
                    )));
                }
                return Err(AppError::CloudDrive(format!(
                    "115 返回错误（HTTP {status}）：{msg}"
                )));
            }
            let list = json["data"].as_array().cloned().unwrap_or_default();
            for item in &list {
                // Directories carry no `fid`; their id is `cid`. Files carry
                // fid + pickcode (needed for download).
                let fid = item["fid"].as_str().unwrap_or("");
                let name = item["n"].as_str().unwrap_or("").to_string();
                if fid.is_empty() {
                    // directory: `cid` may be a JSON string or a number
                    let cid_val = item["cid"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| item["cid"].as_u64().map(|n| n.to_string()))
                        .or_else(|| item["cid"].as_i64().map(|n| n.to_string()))
                        .unwrap_or_default();
                    out.push(crate::commands::CloudDriveFile {
                        fs_id: 0,
                        name,
                        size: 0,
                        path: cid_val.clone(),
                        isdir: true,
                        mtime: Self::parse_mtime(item["t"].as_str().unwrap_or("")),
                        category: 0,
                        id: cid_val,
                        rel_path: String::new(),
                        pdir_fid: String::new(),
                        share_fid_token: String::new(),
                        pickcode: String::new(),
                    });
                } else {
                    let size = item["s"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or_else(|| item["s"].as_u64())
                        .unwrap_or(0);
                    out.push(crate::commands::CloudDriveFile {
                        fs_id: 0,
                        name,
                        size,
                        path: fid.to_string(),
                        isdir: false,
                        mtime: Self::parse_mtime(item["t"].as_str().unwrap_or("")),
                        category: 0,
                        id: fid.to_string(),
                        rel_path: String::new(),
                        pdir_fid: String::new(),
                        share_fid_token: String::new(),
                        pickcode: item["pc"].as_str().unwrap_or("").to_string(),
                    });
                }
            }
            let count = json["count"].as_i64().unwrap_or(0);
            offset += M115_PAGE_SIZE;
            if offset >= count {
                break;
            }
        }
        Ok(out)
    }

    /// Fetch download URLs: m115-encrypt each pickcode, POST downurl, decrypt
    /// the response. The CDN URL requires the account cookie + 115Browser UA.
    pub async fn get_dlinks(
        &mut self,
        _db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let mut result = Vec::new();
        let ua = m115_ua().await;
        for file in files {
            if file.isdir || file.pickcode.is_empty() {
                continue;
            }
            let client = Self::http_client(&ua)?;
            let key = crate::commands::m115_crypto::generate_key();
            let body = serde_json::json!({ "pickcode": file.pickcode }).to_string();
            let data = crate::commands::m115_crypto::encode(body.as_bytes(), &key);
            let url = format!("{M115_DOWN_URL}?t={}", chrono::Utc::now().timestamp());
            let resp = client
                .post(&url)
                .header("Cookie", &self.addition.cookie)
                .form(&[("data", data.as_str())])
                .header("Content-Type", "application/json;charset=UTF-8")
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("请求 115 下载地址失败: {e}")))?;
            let status = resp.status();
            // The CDN rejects the download request (HTTP 403) unless it carries
            // the cookie issued by this downurl response; merge every Set-Cookie
            // into the download header, mirroring the SDK's buildDownloadHeaders.
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
            let plain = crate::commands::m115_crypto::decode(encoded, &key)
                .map_err(|e| AppError::CloudDrive(format!("解密 115 下载响应失败: {e}")))?;
            let plain_json: serde_json::Value = serde_json::from_slice(&plain)
                .map_err(|e| AppError::CloudDrive(format!("解析 115 下载数据失败: {e}")))?;
            // The decrypted payload is `{ <key>: {file_name, file_size, pick_code, url{url}} }`.
            // The SDK iterates the whole map and returns the first valid url
            // (the map key is not guaranteed to be the pickcode), so mirror that:
            // walk object values / arrays / a bare object to find `url.url`.
            fn extract_dlink(v: &serde_json::Value) -> Option<&str> {
                v["url"]["url"].as_str().or_else(|| {
                    v.as_object()
                        .and_then(|o| o.values().find_map(extract_dlink))
                })
            }
            let dlink = extract_dlink(&plain_json)
                .or_else(|| {
                    plain_json
                        .as_array()
                        .and_then(|a| a.iter().find_map(extract_dlink))
                })
                .ok_or_else(|| {
                    let sample = String::from_utf8_lossy(&plain);
                    log::warn!("115 downurl 响应结构异常: {}", &sample[..sample.len().min(400)]);
                    AppError::CloudDrive(format!("115 未返回 {} 的下载地址", file.name))
                })?
                .to_string();
            // The CDN URL is bound to the UA that requested it and requires the
            // account cookie; alist/SDK send only UA + Cookie (no Referer).
            // The downurl-issued CDN cookie(s) are mandatory for the CDN fetch.
            let mut cookie = self.addition.cookie.clone();
            for c in &set_cookies {
                cookie.push_str("; ");
                cookie.push_str(c);
            }
            let headers = vec![
                format!("User-Agent: {ua}"),
                format!("Cookie: {cookie}"),
            ];
            result.push((file.clone(), dlink, headers));
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive("未能获取任何 115 文件的下载地址".into()));
        }
        Ok(result)
    }
}

// ============================== Quark share driver ==============================

/// Quark share driver addition: a shared-folder URL plus the web-session
/// cookie (listing a share needs no login, downloading does). `stoken` is the
/// share access token from `share/sharepage/token`; it is cached and persisted
/// here and refreshed on demand when a call fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarkShareAddition {
    /// Full share URL, e.g. `https://pan.quark.cn/s/<code>` (may carry `?pwd=`).
    pub share_url: String,
    /// pan.quark.cn web-session cookie (download requires login state).
    pub cookie: String,
    /// Optional share passcode. Used only when the URL itself has no `pwd=`.
    #[serde(default)]
    pub passcode: String,
    /// Share access token (`stoken`), refreshed on demand and persisted back.
    #[serde(default)]
    pub stoken: String,
}

impl QuarkShareAddition {
    pub fn parse(json: &str) -> Result<Self, AppError> {
        serde_json::from_str(json)
            .map_err(|e| AppError::CloudDrive(format!("解析驱动配置失败: {e}")))
    }
    fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|e| AppError::CloudDrive(format!("序列化驱动配置失败: {e}")))
    }
    /// Extract `pwd_id` (from `/s/<code>`) and `passcode`. The URL's `?pwd=`
    /// wins when present; otherwise the standalone `passcode` field is used.
    fn credentials(&self) -> Result<(String, String), AppError> {
        let base = self.share_url.split('?').next().unwrap_or("").trim_end_matches('/');
        let pwd_id = base.rsplit('/').next().unwrap_or("").trim().to_string();
        if pwd_id.is_empty() {
            return Err(AppError::CloudDrive(
                "分享地址格式不正确，应为 https://pan.quark.cn/s/<code>".into(),
            ));
        }
        let url_passcode = self
            .share_url
            .split('?')
            .nth(1)
            .unwrap_or("")
            .split('&')
            .find_map(|kv| kv.strip_prefix("pwd="))
            .unwrap_or("")
            .to_string();
        let passcode = if url_passcode.is_empty() {
            self.passcode.trim().to_string()
        } else {
            url_passcode
        };
        Ok((pwd_id, passcode))
    }
}

/// Quark share driver: browse and download from a public share link
/// (`share/sharepage/token` -> `share/sharepage/detail` -> `file/download`).
/// Listing works without a cookie; the download request and the download URL
/// carry the same cookie + share context (stoken/pwd_id/pdir_fid/share token).
///
/// `db` is `Option<&Database>`: mounted storages persist the refreshed stoken
/// back to the row, while transient "提取分享" sessions keep it in memory only.
pub struct QuarkShareDriver {
    id: i64,
    addition: QuarkShareAddition,
}

impl QuarkShareDriver {
    fn http_client() -> Result<reqwest::Client, AppError> {
        reqwest::Client::builder()
            .user_agent(QUARK_UA)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))
    }

    /// Transient driver for the "提取分享" flow: borrows the cookie from the
    /// source storage, no persistence (stoken lives in memory only).
    pub fn transient(share_url: String, passcode: String, cookie: String) -> Self {
        Self {
            id: 0,
            addition: QuarkShareAddition {
                share_url,
                cookie,
                passcode,
                stoken: String::new(),
            },
        }
    }

    async fn persist(&self, db: Option<&Database>) -> Result<(), AppError> {
        if let Some(db) = db {
            db.update_storage_addition(self.id, &self.addition.to_json()?)
                .await?;
        }
        Ok(())
    }

    async fn set_status(&self, db: Option<&Database>, status: &str) -> Result<(), AppError> {
        if let Some(db) = db {
            db.set_storage_status(self.id, status).await?;
        }
        Ok(())
    }

    /// Re-fetch the share access token from `share/sharepage/token`.
    async fn refresh_stoken(&mut self, db: Option<&Database>) -> Result<String, AppError> {
        let (pwd_id, passcode) = self.addition.credentials()?;
        let client = Self::http_client()?;
        let body = serde_json::json!({ "pwd_id": pwd_id, "passcode": passcode });
        let url = format!("{QUARK_API}/share/sharepage/token?pr=ucpro&fr=pc");
        let resp = client
            .post(&url)
            .header("Cookie", &self.addition.cookie)
            .header("Referer", QUARK_REFERER)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("获取分享 token 失败: {e}")))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::CloudDrive(format!("解析分享 token 响应失败: {e}")))?;
        let code = json["code"].as_i64().unwrap_or(-1);
        let message = json["message"].as_str().unwrap_or("");
        if code != 0 {
            return Err(AppError::CloudDrive(format!(
                "获取分享 token 失败（code={code}）：{}",
                if message.is_empty() { "未知错误" } else { message }
            )));
        }
        let stoken = json["data"]["stoken"]
            .as_str()
            .ok_or_else(|| AppError::CloudDrive("分享 token 响应缺少 stoken".into()))?
            .to_string();
        self.addition.stoken = stoken.clone();
        self.persist(db).await?;
        Ok(stoken)
    }

    async fn ensure_stoken(&mut self, db: Option<&Database>) -> Result<String, AppError> {
        if !self.addition.stoken.is_empty() {
            return Ok(self.addition.stoken.clone());
        }
        self.refresh_stoken(db).await
    }

    async fn list(
        &mut self,
        db: Option<&Database>,
        fid: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        const SIZE: usize = 50;
        const MAX_PAGES: usize = 100; // 安全上限：最多 5 万文件
        let (pwd_id, _passcode) = self.addition.credentials()?;
        let mut out = Vec::new();
        let mut page = 1usize;
        let mut total_hint: u64 = 0;
        loop {
            let mut page_ok = false;
            for attempt in 0..QUARK_MAX_ATTEMPTS {
                let stoken = if attempt == 0 {
                    self.ensure_stoken(db).await?
                } else {
                    self.refresh_stoken(db).await?
                };
                let client = Self::http_client()?;
                let url = format!(
                    "{QUARK_API}/share/sharepage/detail?pr=ucpro&fr=pc&pwd_id={}&stoken={}&pdir_fid={}&force=0&_page={}&_size={SIZE}&_fetch_banner=1&_fetch_share=1&_fetch_total=1&_sort=file_type:asc,updated_at:desc",
                    urlencoding::encode(&pwd_id),
                    urlencoding::encode(&stoken),
                    urlencoding::encode(fid),
                    page
                );
                let resp = client
                    .get(&url)
                    .header("Cookie", &self.addition.cookie)
                    .header("Referer", QUARK_REFERER)
                    .send()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("请求夸克分享列表失败: {e}")))?;
                let status = resp.status();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| AppError::CloudDrive(format!("读取夸克分享列表响应失败: {e}")))?;
                let json: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| AppError::CloudDrive(format!("解析夸克分享列表失败: {e}")))?;
                let code = json["code"].as_i64().unwrap_or(-1);
                let message = json["message"].as_str().unwrap_or("");
                if !status.is_success() || code != 0 {
                    if attempt + 1 < QUARK_MAX_ATTEMPTS {
                        continue; // stale stoken or share error → refresh once
                    }
                    return Err(AppError::CloudDrive(format!(
                        "夸克分享返回错误 code={code}: {}",
                        if message.is_empty() { "未知错误" } else { message }
                    )));
                }
                let list = json["data"]["list"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for item in &list {
                    let fid_str = item["fid"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| item["fid"].as_u64().map(|n| n.to_string()))
                        .or_else(|| item["fid"].as_i64().map(|n| n.to_string()));
                    let Some(fid_str) = fid_str else { continue };
                    let is_file = item["file"]
                        .as_bool()
                        .or_else(|| item["file"].as_u64().map(|v| v != 0))
                        .or_else(|| item["dir"].as_bool().map(|d| !d))
                        .unwrap_or(false);
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
                        pdir_fid: fid.to_string(),
                        share_fid_token: item["share_fid_token"].as_str().unwrap_or("").to_string(),
                        pickcode: String::new(),
                    });
                }
                let total = json["metadata"]["_total"].as_u64().unwrap_or(0);
                if total > 0 {
                    total_hint = total;
                }
                page_ok = true;
                break;
            }
            if !page_ok {
                break;
            }
            if page as u64 * SIZE as u64 >= total_hint || page >= MAX_PAGES {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch download URLs from a share. Files are grouped by their parent
    /// fid so each `file/download` request carries one share context
    /// (stoken/pwd_id/pdir_fid/share_fid_token). The signed URL is tied to
    /// the cookie used for the request, so that cookie rides along in aria2.
    pub async fn get_dlinks(
        &mut self,
        db: Option<&Database>,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String, Vec<String>)>, AppError> {
        let (pwd_id, _passcode) = self.addition.credentials()?;
        let mut result = Vec::new();
        let mut by_parent: std::collections::HashMap<String, Vec<crate::commands::CloudDriveFile>> =
            std::collections::HashMap::new();
        for f in files {
            let key = if f.pdir_fid.is_empty() {
                "0".to_string()
            } else {
                f.pdir_fid.clone()
            };
            by_parent.entry(key).or_default().push(f.clone());
        }
        for (pdir_fid, group) in by_parent {
            for chunk in group.chunks(100) {
                let mut chunk_ok = false;
                for attempt in 0..QUARK_MAX_ATTEMPTS {
                    let stoken = if attempt == 0 {
                        self.ensure_stoken(db).await?
                    } else {
                        self.refresh_stoken(db).await?
                    };
                    let client = Self::http_client()?;
                    let fids: Vec<String> = chunk.iter().map(|f| f.id.clone()).collect();
                    let tokens: Vec<String> =
                        chunk.iter().map(|f| f.share_fid_token.clone()).collect();
                    let body = serde_json::json!({
                        "fids": fids,
                        "stoken": stoken,
                        "pwd_id": pwd_id,
                        "pdir_fid": pdir_fid,
                        "share_fid_token": tokens,
                    });
                    let url = format!("{QUARK_API}/file/download?pr=ucpro&fr=pc");
                    let req_cookie = self.addition.cookie.clone();
                    let resp = client
                        .post(&url)
                        .header("Cookie", &req_cookie)
                        .header("Referer", QUARK_REFERER)
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| AppError::CloudDrive(format!("请求夸克分享下载地址失败: {e}")))?;
                    let status = resp.status();
                    let json: serde_json::Value = resp
                        .json()
                        .await
                        .map_err(|e| AppError::CloudDrive(format!("解析夸克分享下载地址失败: {e}")))?;
                    let code = json["code"].as_i64().unwrap_or(-1);
                    let message = json["message"].as_str().unwrap_or("");
                    if !status.is_success() || code != 0 {
                        if attempt + 1 < QUARK_MAX_ATTEMPTS {
                            continue; // refresh stoken and retry once
                        }
                        return Err(AppError::CloudDrive(format!(
                            "获取夸克分享下载地址失败（code={code}）：{}",
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
                    chunk_ok = true;
                    break;
                }
                if !chunk_ok {
                    break;
                }
            }
        }
        if result.is_empty() {
            return Err(AppError::CloudDrive(
                "未能获取任何夸克分享文件的下载地址".into(),
            ));
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

/// The cookie of a quark-family storage (`quark` / `quark_share`), reused by
/// the transient "提取分享" flow for download auth.
fn storage_quark_cookie(storage: &CloudStorage) -> Result<String, AppError> {
    match storage.driver.as_str() {
        "quark" => Ok(QuarkAddition::parse(&storage.addition)?.cookie),
        "quark_share" => Ok(QuarkShareAddition::parse(&storage.addition)?.cookie),
        other => Err(AppError::CloudDrive(format!(
            "「提取分享」仅支持夸克驱动，当前存储为 {other}"
        ))),
    }
}

/// Submit resolved links to aria2 and summarize the outcome.
async fn submit_links(
    app: &AppHandle,
    links: &[(CloudDriveFile, String, Vec<String>)],
    default_dir: Option<&str>,
) -> CloudDriveResult {
    let mut added = 0usize;
    let mut failed = 0usize;
    for (file, dlink, headers) in links {
        // 115 CDN caps concurrent TCP connections per URL (~2 verified live:
        // 2 of 4 parallel Range requests returned 403), so a high split only
        // produces rejected pieces. Keep 115 low; other drive CDNs accept
        // more parallelism. Single-connection throughput on 115 fluctuates
        // (9.6~28MB/s) and is the real speed limit, not connection count.
        let is_115 = dlink.contains("115cdn.net") || dlink.contains(".115.com");
        let split = if is_115 { 4 } else { 32 };
        let mut options = serde_json::json!({
            "header": headers,
            "out": file.name,
            "split": split,
            "max-connection-per-server": split,
            "min-split-size": "1M",
        });
        if let Some(dir) = default_dir
            .and_then(|base| resolve_download_dir(base, &file.rel_path))
        {
            options["dir"] = serde_json::Value::String(dir);
        }
        match submit(app, TaskInput::Uris(vec![dlink.clone()]), options, None).await {
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
    CloudDriveResult {
        message,
        added,
        failed,
    }
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
        CloudDriver::QuarkShare(d) => d.get_dlinks(Some(&db.0), &files).await?,
        CloudDriver::M115(d) => d.get_dlinks(&db.0, &files).await?,
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

    Ok(submit_links(&app, &links, default_dir.as_deref()).await)
}

// ============================== transient share ("提取分享") ==============================

/// Recursively expand checked share folders into files via a transient driver.
/// Same rel_path rules as `expand_folders`; no DB involved.
async fn expand_share_folders(
    driver: &mut QuarkShareDriver,
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
        let children = driver.list(None, &item.path).await?;
        requests += 1;
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

/// Browse a share without adding a storage: borrows the source storage's
/// cookie for auth and keeps stoken in memory.
#[tauri::command]
pub async fn cloud_drive_share_list(
    db: State<'_, DatabaseState>,
    storage_id: i64,
    share_url: String,
    passcode: String,
    path: String,
) -> Result<Vec<CloudDriveFile>, AppError> {
    let storage = db
        .0
        .get_storage(storage_id)
        .await?
        .ok_or_else(|| AppError::CloudDrive("存储不存在".into()))?;
    if storage.disabled {
        return Err(AppError::CloudDrive("该存储已被禁用".into()));
    }
    let cookie = storage_quark_cookie(&storage)?;
    let mut driver = QuarkShareDriver::transient(share_url, passcode, cookie);
    driver.list(None, &path).await
}

/// Download checked files from a transient share (提取分享).
#[tauri::command]
pub async fn cloud_drive_share_download(
    app: AppHandle,
    db: State<'_, DatabaseState>,
    storage_id: i64,
    share_url: String,
    passcode: String,
    files: Vec<CloudDriveFile>,
) -> Result<CloudDriveResult, AppError> {
    if files.is_empty() {
        return Err(AppError::CloudDrive("没有选择文件".into()));
    }
    let storage = db
        .0
        .get_storage(storage_id)
        .await?
        .ok_or_else(|| AppError::CloudDrive("存储不存在".into()))?;
    if storage.disabled {
        return Err(AppError::CloudDrive("该存储已被禁用".into()));
    }
    let cookie = storage_quark_cookie(&storage)?;
    let mut driver = QuarkShareDriver::transient(share_url, passcode, cookie);
    let files = expand_share_folders(&mut driver, files).await?;
    if files.is_empty() {
        return Err(AppError::CloudDrive("所选文件夹中没有可下载的文件".into()));
    }
    let links = driver.get_dlinks(None, &files).await?;
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
    Ok(submit_links(&app, &links, default_dir.as_deref()).await)
}


// ============================== quark qrcode login (扫码获取 Cookie) ==============================
//
// Mirrors xiaoya-alist's `quark_cookie.py`: generates a QR code via the
// uop.quark.cn CAS endpoints, polls until the Quark app scans it, then trades
// the service ticket for account cookies + the drive-pc `__puus` cookie.

const QUARK_UOP_CAS: &str = "https://uop.quark.cn/cas/ajax";
const QUARK_UOP_CLIENT_ID: &str = "532";

/// Generated QR login session handed to the frontend for display/polling.
#[derive(serde::Serialize)]
pub struct QuarkQrcodeCreated {
    pub token: String,
    pub qrcode_svg: String,
}

/// Poll result. `state`: success / pending / expired / error.
#[derive(serde::Serialize)]
pub struct QuarkQrcodeStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn quark_uop_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Step 1: create a QR login token and render the QR code as SVG.
#[tauri::command]
pub async fn cloud_drive_quark_qrcode_create() -> Result<QuarkQrcodeCreated, AppError> {
    let client = reqwest::Client::builder()
        .user_agent(QUARK_UA)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))?;
    let url = format!(
        "{QUARK_UOP_CAS}/getTokenForQrcodeLogin?client_id={QUARK_UOP_CLIENT_ID}&v=1.2&request_id={}",
        quark_uop_request_id()
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求夸克登录二维码失败: {e}")))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析夸克登录二维码响应失败: {e}")))?;
    let token = json["data"]["members"]["token"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("夸克登录二维码响应缺少 token".into()))?
        .to_string();
    let qr_content = format!(
        "https://su.quark.cn/4_eMHBJ?token={token}&client_id={QUARK_UOP_CLIENT_ID}&ssb=weblogin&uc_param_str=&uc_biz_str=S%3Acustom%7COPT%3ASAREA%400%7COPT%3AIMMERSIVE%401%7COPT%3ABACK_BTN_STYLE%400"
    );
    let code = qrcode::QrCode::new(qr_content.as_bytes())
        .map_err(|e| AppError::CloudDrive(format!("生成二维码失败: {e}")))?;
    let qrcode_svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .build();
    Ok(QuarkQrcodeCreated { token, qrcode_svg })
}

/// Collect `Set-Cookie` response headers into an ordered name=value list.
fn collect_set_cookie(headers: &reqwest::header::HeaderMap, out: &mut Vec<(String, String)>) {
    for value in headers.get_all(reqwest::header::SET_COOKIE) {
        let Ok(s) = value.to_str() else { continue };
        let Some((name, rest)) = s.split_once('=') else { continue };
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let value = rest.split(';').next().unwrap_or("").trim().to_string();
        // Later responses win over earlier ones.
        if let Some(slot) = out.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            out.push((name, value));
        }
    }
}

/// Step 2: poll the scan status; on success returns the merged cookie string
/// (account cookies from `pan.quark.cn/account/info` + drive-pc `__puus`).
#[tauri::command]
pub async fn cloud_drive_quark_qrcode_status(
    token: String,
) -> Result<QuarkQrcodeStatus, AppError> {
    let client = reqwest::Client::builder()
        .user_agent(QUARK_UA)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))?;
    let url = format!(
        "{QUARK_UOP_CAS}/getServiceTicketByQrcodeToken?client_id={QUARK_UOP_CLIENT_ID}&v=1.2&token={}&request_id={}",
        urlencoding::encode(&token),
        quark_uop_request_id()
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("查询夸克登录状态失败: {e}")))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析夸克登录状态失败: {e}")))?;
    let status = json["status"].as_i64().unwrap_or(-1);
    match status {
        2000000 => {
            let ticket = json["data"]["members"]["service_ticket"]
                .as_str()
                .ok_or_else(|| AppError::CloudDrive("扫码成功但缺少 service_ticket".into()))?;
            // Trade the ticket for account cookies (USID/USESSION/...).
            let info_url = format!(
                "https://pan.quark.cn/account/info?st={}&lw=scan",
                urlencoding::encode(ticket)
            );
            let info = client
                .get(&info_url)
                .header("Referer", "https://pan.quark.cn")
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("换取夸克账号 cookie 失败: {e}")))?;
            if !info.status().is_success() {
                return Ok(QuarkQrcodeStatus {
                    state: "error".into(),
                    cookie: None,
                    message: Some(format!("换取夸克账号 cookie 失败（HTTP {}）", info.status())),
                });
            }
            let mut cookies: Vec<(String, String)> = Vec::new();
            collect_set_cookie(info.headers(), &mut cookies);
            // Drive-pc config reissues the session cookie (`__puus`); it must
            // carry the account cookies from the previous step.
            let account_cookie = cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            let config_resp = client
                .get("https://drive-pc.quark.cn/1/clouddrive/config?pr=ucpro&fr=pc&uc_param_str=")
                .header("Referer", "https://pan.quark.cn")
                .header("Cookie", &account_cookie)
                .send()
                .await;
            match config_resp {
                Ok(r) => collect_set_cookie(r.headers(), &mut cookies),
                Err(e) => log::warn!("cloud-drive: 获取 __puus 失败: {e}"),
            }
            if cookies.is_empty() {
                return Ok(QuarkQrcodeStatus {
                    state: "error".into(),
                    cookie: None,
                    message: Some("未从登录响应中获取到任何 cookie".into()),
                });
            }
            let cookie_str = cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            Ok(QuarkQrcodeStatus {
                state: "success".into(),
                cookie: Some(cookie_str),
                message: None,
            })
        }
        50004002 => Ok(QuarkQrcodeStatus {
            state: "expired".into(),
            cookie: None,
            message: Some("二维码已过期，请刷新后重试".into()),
        }),
        50004001 => Ok(QuarkQrcodeStatus {
            state: "pending".into(),
            cookie: None,
            message: None,
        }),
        other => Ok(QuarkQrcodeStatus {
            state: "error".into(),
            cookie: None,
            message: Some(format!("夸克返回未知状态: {other}")),
        }),
    }
}

// ============================== 115 qrcode login (扫码获取 Cookie) ==============================
//
// Mirrors 115driver's qrcode.go: create a session, render the QR code,
// poll until the 115 app scans it, then trade `account=uid` for the
// UID/CID/SEID/KID credential cookie.

/// Generated 115 QR login session handed to the frontend for display/polling.
#[derive(serde::Serialize)]
pub struct M115QrcodeCreated {
    pub qrcode_svg: String,
    pub uid: String,
    pub time: i64,
    pub sign: String,
}

/// Poll result. `state`: success / pending / scanned / expired / canceled / error.
#[derive(serde::Serialize)]
pub struct M115QrcodeStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Step 1: create a 115 QR login session and render the QR code as SVG.
#[tauri::command]
pub async fn cloud_drive_115_qrcode_create() -> Result<M115QrcodeCreated, AppError> {
    let client = reqwest::Client::builder()
        .user_agent(format!("Mozilla/5.0 115Browser/{M115_UA_FALLBACK}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))?;
    let resp = client
        .get(M115_QRCODE_TOKEN_URL)
        .header("Referer", M115_REFERER)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求 115 登录二维码失败: {e}")))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析 115 登录二维码响应失败: {e}")))?;
    let data = &json["data"];
    let qr_content = data["qrcode"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("115 登录二维码响应缺少 qrcode".into()))?;
    let uid = data["uid"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("115 登录二维码响应缺少 uid".into()))?
        .to_string();
    let time = data["time"].as_i64().unwrap_or(0);
    let sign = data["sign"]
        .as_str()
        .ok_or_else(|| AppError::CloudDrive("115 登录二维码响应缺少 sign".into()))?
        .to_string();
    let code = qrcode::QrCode::new(qr_content.as_bytes())
        .map_err(|e| AppError::CloudDrive(format!("生成二维码失败: {e}")))?;
    let qrcode_svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(320, 320)
        .build();
    Ok(M115QrcodeCreated {
        qrcode_svg,
        uid,
        time,
        sign,
    })
}

/// Step 2: poll the scan status; on success returns the merged cookie string
/// (`UID=..;CID=..;SEID=..;KID=..`) from the login endpoint.
#[tauri::command]
pub async fn cloud_drive_115_qrcode_status(
    uid: String,
    time: i64,
    sign: String,
) -> Result<M115QrcodeStatus, AppError> {
    let client = reqwest::Client::builder()
        .user_agent(format!("Mozilla/5.0 115Browser/{M115_UA_FALLBACK}"))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))?;
    let url = format!(
        "{M115_QRCODE_STATUS_URL}?uid={}&time={}&sign={}&_={}",
        urlencoding::encode(&uid),
        time,
        urlencoding::encode(&sign),
        chrono::Utc::now().timestamp()
    );
    let resp = client
        .get(&url)
        .header("Referer", M115_REFERER)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("查询 115 登录状态失败: {e}")))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::CloudDrive(format!("解析 115 登录状态失败: {e}")))?;
    let status = json["data"]["status"].as_i64().unwrap_or(-1);
    match status {
        2 => {
            // 已确认：用 account=uid 换取登录凭据
            let login_resp = client
                .post(M115_QRCODE_LOGIN_URL)
                .header("Referer", M115_REFERER)
                .form(&[("account", uid.as_str()), ("app", "web")])
                .send()
                .await
                .map_err(|e| AppError::CloudDrive(format!("换取 115 登录凭据失败: {e}")))?;
            let login_json: serde_json::Value = login_resp
                .json()
                .await
                .map_err(|e| AppError::CloudDrive(format!("解析 115 登录凭据失败: {e}")))?;
            let cr = &login_json["data"]["cookie"];
            let cred_uid = cr["UID"].as_str().unwrap_or("");
            let cred_cid = cr["CID"].as_str().unwrap_or("");
            let cred_seid = cr["SEID"].as_str().unwrap_or("");
            let cred_kid = cr["KID"].as_str().unwrap_or("");
            if cred_uid.is_empty() || cred_cid.is_empty() || cred_seid.is_empty() {
                return Ok(M115QrcodeStatus {
                    state: "error".into(),
                    cookie: None,
                    message: Some(format!(
                        "115 登录凭据不完整（UID/CID/SEID 缺失）：{}",
                        login_json["error"].as_str().unwrap_or("")
                    )),
                });
            }
            let mut cookie = format!("UID={cred_uid};CID={cred_cid};SEID={cred_seid}");
            if !cred_kid.is_empty() {
                cookie.push_str(&format!(";KID={cred_kid}"));
            }
            Ok(M115QrcodeStatus {
                state: "success".into(),
                cookie: Some(cookie),
                message: None,
            })
        }
        1 => Ok(M115QrcodeStatus {
            state: "scanned".into(),
            cookie: None,
            message: Some("已扫码，请在手机上确认登录".into()),
        }),
        0 => Ok(M115QrcodeStatus {
            state: "pending".into(),
            cookie: None,
            message: None,
        }),
        -1 => Ok(M115QrcodeStatus {
            state: "expired".into(),
            cookie: None,
            message: Some("二维码已过期，请刷新后重试".into()),
        }),
        -2 => Ok(M115QrcodeStatus {
            state: "canceled".into(),
            cookie: None,
            message: Some("已取消登录".into()),
        }),
        other => Ok(M115QrcodeStatus {
            state: "error".into(),
            cookie: None,
            message: Some(format!("115 返回未知状态: {other}")),
        }),
    }
}
