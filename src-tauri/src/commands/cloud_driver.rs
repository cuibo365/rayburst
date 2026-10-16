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

/// A materialized driver instance (enum dispatch keeps this dependency-free).
pub enum CloudDriver {
    Baidu(BaiduDriver),
}

impl CloudDriver {
    pub fn from_storage(storage: &CloudStorage) -> Result<Self, AppError> {
        match storage.driver.as_str() {
            "baidu_netdisk" => Ok(CloudDriver::Baidu(BaiduDriver {
                id: storage.id,
                addition: BaiduAddition::parse(&storage.addition)?,
            })),
            other => Err(AppError::CloudDrive(format!(
                "不支持的驱动类型：{other}"
            ))),
        }
    }

    pub fn driver_name(&self) -> &'static str {
        match self {
            CloudDriver::Baidu(_) => "baidu_netdisk",
        }
    }

    /// Resolve a fresh access_token (renew and persist if expired).
    pub async fn access_token(&mut self, db: &Database) -> Result<String, AppError> {
        match self {
            CloudDriver::Baidu(d) => d.ensure_access_token(db).await,
        }
    }

    /// List the children of `path` (absolute, including the mount root).
    pub async fn list(
        &mut self,
        db: &Database,
        path: &str,
    ) -> Result<Vec<crate::commands::CloudDriveFile>, AppError> {
        let token = self.access_token(db).await?;
        let result = match self {
            CloudDriver::Baidu(d) => d.list(path, &token).await,
        };
        match self {
            CloudDriver::Baidu(d) => {
                match &result {
                    Ok(_) => {
                        d.set_status(db, "正常").await;
                    }
                    Err(e) => {
                        d.set_status(db, &format!("错误: {e}")).await;
                    }
                }
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
    pub async fn get_dlinks(
        &mut self,
        db: &Database,
        files: &[crate::commands::CloudDriveFile],
    ) -> Result<Vec<(crate::commands::CloudDriveFile, String)>, AppError> {
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
                        result.push((file.clone(), dlink));
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

// ============================== storage-scoped commands ==============================

use crate::commands::cloud_drive::{resolve_download_dir, sanitize_dir_path, CloudDriveFile, CloudDriveResult};
use crate::services::downloads::{submit, TaskInput};

/// Recursively expand checked folders into contained files via the driver.
async fn expand_folders(
    driver: &mut CloudDriver,
    db: &Database,
    files: Vec<CloudDriveFile>,
) -> Result<Vec<CloudDriveFile>, AppError> {
    let mut result: Vec<CloudDriveFile> = Vec::new();
    let mut queue: Vec<CloudDriveFile> = files;
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
        queue.extend(children);
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
    for (file, dlink) in &links {
        let mut options = serde_json::json!({
            "header": [
                format!("User-Agent: {USER_AGENT}"),
                format!("Referer: {PAN_REFERER}"),
            ],
            "out": file.name,
        });
        if let Some(dir) = default_dir
            .as_deref()
            .and_then(|base| resolve_download_dir(base, current_dir.as_deref(), &file.path))
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

