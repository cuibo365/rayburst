//! Cloud drive (网盘) shared types & helpers.
//!
//! The driver architecture lives in `commands/cloud_driver.rs` (storage-based,
//! alist-style). This module keeps only the shared file/result models and the
//! download-path helpers that the driver layer reuses.

use serde::{Deserialize, Serialize};

/// A file checked by the user in a drive page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudDriveFile {
    pub fs_id: u64,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub path: String,
    /// True for folders; the backend expands them into their contained files.
    #[serde(default)]
    pub isdir: bool,
    /// Last-modified unix timestamp (seconds), from the xpan list API.
    #[serde(default)]
    pub mtime: u64,
    /// Baidu file category (1 video, 2 audio, 3 image, 4 doc, 5 app, 6 other, 7 torrent).
    #[serde(default)]
    pub category: u32,
}

#[derive(Debug, Serialize)]
pub struct CloudDriveResult {
    pub message: String,
    pub added: usize,
    pub failed: usize,
}

/// Compute the aria2 `dir` for a drive file:
///   default download dir + (file path minus the current browser folder path)
/// Returns `None` when there is no default dir, no current dir, or the file
/// path does not live under the current folder (fall back to engine default).
pub(crate) fn resolve_download_dir(
    default_dir: &str,
    current_dir: Option<&str>,
    file_path: &str,
) -> Option<String> {
    let base = default_dir.trim();
    if base.is_empty() {
        return None;
    }
    let cur = current_dir.map(str::trim).filter(|s| !s.is_empty())?;
    let fp = file_path.trim_start_matches('/');
    let rest = fp.strip_prefix(cur.trim_end_matches('/').trim_start_matches('/'))?;
    let rel = rest.trim_start_matches('/');
    let joined = if rel.is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches(['/', '\\']), rel)
    };
    Some(sanitize_dir_path(&joined))
}

/// Sanitize a joined download path:
/// - collapse `//` -> `/` and `\\` -> `\`
/// - normalize `\` -> `/` (aria2 accepts `D:/...` on Windows)
/// - drop Windows-illegal chars `< > " | ? *` (and stray `:`), control chars,
///   while keeping the drive-letter colon (`D:/`)
/// - drop `.` / `..` segments so the result cannot escape the base dir
pub(crate) fn sanitize_dir_path(raw: &str) -> String {
    let mut s = raw.replace("\\\\", "\\");
    while s.contains("\\\\") {
        s = s.replace("\\\\", "\\");
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    s = s.replace('\\', "/");
    while s.contains("//") {
        s = s.replace("//", "/");
    }

    let keep_drive = (s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic())
        .then(|| s[..2].to_string());
    let body = if let Some(drive) = &keep_drive {
        &s[2..]
    } else {
        s.as_str()
    };
    let cleaned: String = body
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | ':') && !c.is_control())
        .collect();

    let segments: Vec<&str> = cleaned
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect();

    let mut out = String::new();
    if let Some(drive) = &keep_drive {
        out.push_str(drive);
        out.push('/');
    }
    out.push_str(&segments.join("/"));
    out
}
