//! Cloud storage management commands (alist-style storage CRUD).
use crate::database::{CloudStorage, DatabaseState};
use crate::error::AppError;
use tauri::State;

#[tauri::command]
pub async fn cloud_storage_list(
    state: State<'_, DatabaseState>,
) -> Result<Vec<CloudStorage>, AppError> {
    state.0.list_storages().await
}

#[tauri::command]
pub async fn cloud_storage_add(
    state: State<'_, DatabaseState>,
    mount_path: String,
    driver: String,
    addition: String,
    remark: String,
) -> Result<CloudStorage, AppError> {
    state
        .0
        .add_storage(&mount_path, &driver, &addition, &remark)
        .await
}

#[tauri::command]
pub async fn cloud_storage_update(
    state: State<'_, DatabaseState>,
    id: i64,
    mount_path: Option<String>,
    addition: Option<String>,
    remark: Option<String>,
) -> Result<Option<CloudStorage>, AppError> {
    state
        .0
        .update_storage(
            id,
            mount_path.as_deref(),
            addition.as_deref(),
            remark.as_deref(),
        )
        .await
}

#[tauri::command]
pub async fn cloud_storage_delete(
    state: State<'_, DatabaseState>,
    id: i64,
) -> Result<bool, AppError> {
    state.0.delete_storage(id).await
}

#[tauri::command]
pub async fn cloud_storage_toggle(
    state: State<'_, DatabaseState>,
    id: i64,
    disabled: bool,
) -> Result<Option<CloudStorage>, AppError> {
    state.0.set_storage_disabled(id, disabled).await
}

/// Export all storages to a JSON file (mount_path/driver/addition/remark/
/// disabled/sort_order). Local-only fields (id/status/timestamps) are omitted
/// and rebuilt on import.
#[tauri::command]
pub async fn cloud_storage_export(
    state: State<'_, DatabaseState>,
    path: String,
) -> Result<usize, AppError> {
    let storages = state.0.list_storages().await?;
    let items: Vec<serde_json::Value> = storages
        .iter()
        .map(|s| {
            serde_json::json!({
                "mount_path": s.mount_path,
                "driver": s.driver,
                "addition": s.addition,
                "remark": s.remark,
                "disabled": s.disabled,
                "sort_order": s.sort_order,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "version": 1,
        "exported_at": chrono::Local::now().to_rfc3339(),
        "storages": items,
    });
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|e| AppError::Database(format!("序列化导出数据失败: {e}")))?;
    std::fs::write(&path, text)
        .map_err(|e| AppError::Database(format!("写入导出文件失败（{path}）：{e}")))?;
    Ok(storages.len())
}

/// Import storages from an exported JSON file. Entries whose mount_path
/// already exists are skipped; the rest are added via the normal add path
/// (addition must be a valid JSON object). Returns per-entry outcomes.
#[tauri::command]
pub async fn cloud_storage_import(
    state: State<'_, DatabaseState>,
    path: String,
) -> Result<serde_json::Value, AppError> {
    let text = std::fs::read_to_string(&path)
        .map_err(|e| AppError::Database(format!("读取导入文件失败（{path}）：{e}")))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AppError::Database(format!("导入文件不是有效 JSON：{e}")))?;
    let storages = v["storages"]
        .as_array()
        .ok_or_else(|| AppError::Database("导入文件缺少 storages 数组".into()))?;

    let existing: std::collections::HashSet<String> = state
        .0
        .list_storages()
        .await?
        .into_iter()
        .map(|s| s.mount_path)
        .collect();

    let mut imported = 0usize;
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    for item in storages {
        let mount_path = item["mount_path"].as_str().unwrap_or("").trim();
        let driver = item["driver"].as_str().unwrap_or("").trim();
        let addition = item["addition"].as_str().unwrap_or("{}");
        let remark = item["remark"].as_str().unwrap_or("");
        if mount_path.is_empty() {
            skipped.push(serde_json::json!({"mount_path": "", "reason": "缺少挂载路径"}));
            continue;
        }
        if driver.is_empty() {
            skipped.push(serde_json::json!({"mount_path": mount_path, "reason": "缺少驱动"}));
            continue;
        }
        if existing.contains(mount_path) {
            skipped.push(serde_json::json!({"mount_path": mount_path, "reason": "挂载路径已存在"}));
            continue;
        }
        match state
            .0
            .add_storage(mount_path, driver, addition, remark)
            .await
        {
            Ok(_) => {
                imported += 1;
                existing.insert(mount_path.to_string());
            }
            Err(e) => {
                skipped.push(serde_json::json!({"mount_path": mount_path, "reason": e.to_string()}));
            }
        }
    }
    Ok(serde_json::json!({ "imported": imported, "skipped": skipped }))
}
