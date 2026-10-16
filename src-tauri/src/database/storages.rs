//! Cloud drive storages (alist-style driver mounts).
//!
//! One row per mounted driver instance. Driver-specific configuration lives in
//! the `addition` column as JSON (e.g. `{"refresh_token":"...","access_token":"...","root_path":"/"}`),
//! matching how `http_auth_credentials` stores opaque credentials.
use super::Database;
use crate::error::AppError;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CloudStorage {
    pub id: i64,
    pub mount_path: String,
    pub driver: String,
    pub addition: String,
    pub status: String,
    pub disabled: bool,
    pub remark: String,
    pub sort_order: i64,
    pub created_at: String,
    pub modified: String,
}

fn row_to_storage(row: &rusqlite::Row<'_>) -> rusqlite::Result<CloudStorage> {
    Ok(CloudStorage {
        id: row.get(0)?,
        mount_path: row.get(1)?,
        driver: row.get(2)?,
        addition: row.get(3)?,
        status: row.get(4)?,
        disabled: row.get::<_, i64>(5)? != 0,
        remark: row.get(6)?,
        sort_order: row.get(7)?,
        created_at: row.get(8)?,
        modified: row.get(9)?,
    })
}

const COLUMNS: &str = "id,mount_path,driver,addition,status,disabled,remark,sort_order,created_at,modified";

fn valid_mount_path(path: &str) -> Result<(), AppError> {
    if path.trim().is_empty() {
        return Err(AppError::Database("挂载路径不能为空".into()));
    }
    Ok(())
}

impl Database {
    /// All storages ordered by sort_order then id (stable for the home list).
    pub async fn list_storages(&self) -> Result<Vec<CloudStorage>, AppError> {
        let conn = self.connection().await?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM cloud_storages ORDER BY sort_order ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], row_to_storage)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub async fn get_storage(&self, id: i64) -> Result<Option<CloudStorage>, AppError> {
        Ok(self
            .connection()
            .await?
            .query_row(
                &format!("SELECT {COLUMNS} FROM cloud_storages WHERE id=?1"),
                [id],
                row_to_storage,
            )
            .optional()?)
    }

    pub async fn add_storage(
        &self,
        mount_path: &str,
        driver: &str,
        addition: &str,
        remark: &str,
    ) -> Result<CloudStorage, AppError> {
        // Store the mount path exactly as provided (frontend trims/validates).
        valid_mount_path(mount_path)?;
        if driver.trim().is_empty() {
            return Err(AppError::Database("驱动不能为空".into()));
        }
        // Friendly duplicate check (DB UNIQUE constraint stays as backstop).
        if self
            .connection()
            .await?
            .query_row(
                "SELECT 1 FROM cloud_storages WHERE mount_path=?1",
                [mount_path],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            return Err(AppError::Database(format!(
                "挂载路径已存在：{mount_path}"
            )));
        }
        // Validate that addition is a JSON object before persisting.
        let addition = addition.trim();
        let parsed: serde_json::Value = serde_json::from_str(addition)
            .map_err(|e| AppError::Database(format!("addition 不是有效 JSON：{e}")))?;
        if !parsed.is_object() {
            return Err(AppError::Database("addition 必须是 JSON 对象".into()));
        }
        self.connection().await?.execute(
            "INSERT INTO cloud_storages(mount_path,driver,addition,remark) VALUES (?1,?2,?3,?4)",
            params![mount_path, driver, addition, remark.trim()],
        )?;
        Ok(self
            .connection()
            .await?
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM cloud_storages WHERE mount_path=?1"
                ),
                [mount_path],
                row_to_storage,
            )?)
    }

    /// Update editable fields (mount_path/addition/remark). Pass None to keep a field.
    pub async fn update_storage(
        &self,
        id: i64,
        mount_path: Option<&str>,
        addition: Option<&str>,
        remark: Option<&str>,
    ) -> Result<Option<CloudStorage>, AppError> {
        let current = self.get_storage(id).await?;
        let Some(current) = current else {
            return Ok(None);
        };
        let new_path = match mount_path {
            Some(p) => {
                // Store exactly as provided (frontend trims/validates).
                valid_mount_path(p)?;
                // Duplicate check excluding this row (DB UNIQUE is the backstop).
                if p != current.mount_path
                    && self
                        .connection()
                        .await?
                        .query_row(
                            "SELECT 1 FROM cloud_storages WHERE mount_path=?1 AND id<>?2",
                            params![p, id],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some()
                {
                    return Err(AppError::Database(format!("挂载路径已存在：{p}")));
                }
                p.to_string()
            }
            None => current.mount_path.clone(),
        };
        let new_addition = match addition {
            Some(a) => {
                let a = a.trim();
                let parsed: serde_json::Value = serde_json::from_str(a)
                    .map_err(|e| AppError::Database(format!("addition 不是有效 JSON：{e}")))?;
                if !parsed.is_object() {
                    return Err(AppError::Database("addition 必须是 JSON 对象".into()));
                }
                a.to_string()
            }
            None => current.addition.clone(),
        };
        let new_remark = match remark {
            Some(r) => r.trim().to_string(),
            None => current.remark.clone(),
        };
        self.connection().await?.execute(
            "UPDATE cloud_storages SET mount_path=?1, addition=?2, remark=?3, modified=CURRENT_TIMESTAMP WHERE id=?4",
            params![new_path, new_addition, new_remark, id],
        )?;
        Ok(self.get_storage(id).await?)
    }

    pub async fn delete_storage(&self, id: i64) -> Result<bool, AppError> {
        let affected = self.connection().await?.execute(
            "DELETE FROM cloud_storages WHERE id=?1",
            [id],
        )?;
        Ok(affected > 0)
    }

    pub async fn set_storage_disabled(&self, id: i64, disabled: bool) -> Result<Option<CloudStorage>, AppError> {
        let affected = self.connection().await?.execute(
            "UPDATE cloud_storages SET disabled=?1, modified=CURRENT_TIMESTAMP WHERE id=?2",
            params![if disabled { 1 } else { 0 }, id],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(self.get_storage(id).await?)
    }

    /// Replace the stored addition (used when a driver refreshes its access_token)
    /// and refresh the modified timestamp.
    pub async fn update_storage_addition(&self, id: i64, addition: &str) -> Result<Option<CloudStorage>, AppError> {
        let affected = self.connection().await?.execute(
            "UPDATE cloud_storages SET addition=?1, modified=CURRENT_TIMESTAMP WHERE id=?2",
            params![addition, id],
        )?;
        if affected == 0 {
            return Ok(None);
        }
        Ok(self.get_storage(id).await?)
    }

    pub async fn set_storage_status(&self, id: i64, status: &str) -> Result<(), AppError> {
        self.connection().await?.execute(
            "UPDATE cloud_storages SET status=?1 WHERE id=?2",
            params![status, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn storage_crud_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let s = db
            .add_storage("/百度网盘", "baidu_netdisk", r#"{"refresh_token":"t1","access_token":"a1","root_path":"/"}"#, "主盘")
            .await
            .unwrap();
        assert_eq!(s.mount_path, "/百度网盘");
        assert!(!s.disabled);
        // duplicate mount_path rejected
        assert!(db
            .add_storage("/百度网盘", "baidu_netdisk", "{}", "")
            .await
            .is_err());
        // invalid addition rejected
        assert!(db
            .add_storage("/夸克", "quark_uc", "not-json", "")
            .await
            .is_err());
        // update
        let u = db
            .update_storage(s.id, Some("/百度主盘"), None, Some("新备注"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(u.mount_path, "/百度主盘");
        assert_eq!(u.remark, "新备注");
        // addition refresh
        let refreshed = db
            .update_storage_addition(s.id, r#"{"refresh_token":"t2","access_token":"a2","root_path":"/"}"#)
            .await
            .unwrap()
            .unwrap();
        assert!(refreshed.addition.contains("a2"));
        // disable / enable
        assert!(db
            .set_storage_disabled(s.id, true)
            .await
            .unwrap()
            .unwrap()
            .disabled);
        assert!(!db
            .set_storage_disabled(s.id, false)
            .await
            .unwrap()
            .unwrap()
            .disabled);
        // delete
        assert!(db.delete_storage(s.id).await.unwrap());
        assert!(db.get_storage(s.id).await.unwrap().is_none());
    }
}
