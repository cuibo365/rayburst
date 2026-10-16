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
