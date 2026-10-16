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
    /// Universal string item id. Baidu: decimal fs_id; Quark: the 32-char hex
    /// fid (does not fit u64). Selection and download lookups use this.
    #[serde(default)]
    pub id: String,
    /// Relative dir chain rooted at the checked (level-1) item, e.g. `A/B` for
    /// a file inside checked folder `A/B`. Empty for level-1 files (flat
    /// download into the default dir). Populated by `expand_folders`.
    #[serde(default)]
    pub rel_path: String,
    /// Parent directory id. Quark share: the containing share folder fid
    /// (`0` = share root), carried back in the share download request.
    #[serde(default)]
    pub pdir_fid: String,
    /// Share-scoped file token returned by the share detail API; echoed back
    /// in the share download request. Empty for non-share drivers.
    #[serde(default)]
    pub share_fid_token: String,
}

#[derive(Debug, Serialize)]
pub struct CloudDriveResult {
    pub message: String,
    pub added: usize,
    pub failed: usize,
}

/// Compute the aria2 `dir` for a drive file from its `rel_path` chain
/// (level-1 checked item as root):
///   default download dir + rel_path
/// An empty rel_path (level-1 file) resolves to the default dir itself.
pub(crate) fn resolve_download_dir(default_dir: &str, rel_path: &str) -> Option<String> {
    let base = default_dir.trim();
    if base.is_empty() {
        return None;
    }
    let rel = rel_path.trim_matches('/');
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
    let body = if keep_drive.is_some() {
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
