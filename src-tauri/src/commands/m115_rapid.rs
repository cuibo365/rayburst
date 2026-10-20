//! 115 rapid upload (秒传/闪传) — port of SheltonZhu/115driver
//! (`pkg/crypto/ec115` + `pkg/driver/upload.go`).
//!
//! A file already present in the 115 library is "rapid uploaded" by submitting
//! its full SHA1 (QuickID) to `ApiUploadInit`; a hit (`status==2`) returns a
//! pickcode without transferring any bytes and without consuming the monthly
//! cloud-download quota. The pickcode feeds the normal `downurl` flow to get a
//! CDN link.
//!
//! Request envelope: ECDH (P-224, fixed remote key) → shared secret →
//! AES-128-ECB "XOR-CFB" encryption of the form body, plus a signed
//! `k_ec` token; the response is AES-128-CBC + LZ4 compressed.

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use base64::Engine;
use p224::ecdh::EphemeralSecret;
use p224::elliptic_curve::sec1::ToEncodedPoint;
use p224::PublicKey;
use rand::RngCore;
use sha1::Sha1;
use sha2::Digest;

use crate::error::AppError;

/// Upload endpoint (SDK `ApiUploadInit`).
const API_UPLOAD_INIT: &str = "https://uplb.115.com/4.0/initupload.php";


/// Remote public key (P-224, uncompressed X||Y, 56 bytes) from the SDK.
const REMOTE_PUB_KEY: [u8; 56] = [
    0x57, 0xA2, 0x92, 0x57, 0xCD, 0x23, 0x20, 0xE5, 0xD6, 0xD1, 0x43, 0x32, 0x2F, 0xA4, 0xBB,
    0x8A, 0x3C, 0xF9, 0xD3, 0xCC, 0x62, 0x3E, 0xF5, 0xED, 0xAC, 0x62, 0xB7, 0x67, 0x8A, 0x89,
    0xC9, 0x1A, 0x83, 0xBA, 0x80, 0x0D, 0x61, 0x29, 0xF5, 0x22, 0xD0, 0x34, 0xC8, 0x95, 0xDD,
    0x24, 0x65, 0x24, 0x3A, 0xDD, 0xC2, 0x50, 0x95, 0x3B, 0xEE, 0xBA,
];

const CRC_SALT: &str = "^j>WD3Kr?J2gLFjD4W2y@";
const MD5_SALT: &str = "Qclm8MGWUv59TnrR0XPg";
const APP_VERSION: &str = "36.0.1";

/// ECDH cipher mirroring the Go `ec115.EcdhCipher`.
struct EcdhCipher {
    key: [u8; 16],
    iv: [u8; 16],
    /// Compressed local public key with a TL prefix: `[0x1D, 0x02|0x03, X(28)]`.
    pub_key: Vec<u8>,
}

impl EcdhCipher {
    fn new() -> Result<Self, AppError> {
        let mut rng = rand::rngs::OsRng;
        let secret = EphemeralSecret::random(&mut rng);
        let local_pk = PublicKey::from(&secret);

        let mut sec1 = Vec::with_capacity(57);
        sec1.push(0x04);
        sec1.extend_from_slice(&REMOTE_PUB_KEY[0..28]);
        sec1.extend_from_slice(&REMOTE_PUB_KEY[28..56]);
        let remote_pk = PublicKey::from_sec1_bytes(&sec1)
            .map_err(|_| AppError::CloudDrive("115 ECDH 远程公钥解析失败".into()))?;

        let shared = secret.diffie_hellman(&remote_pk);
        let s = shared.raw_secret_bytes();
        let mut key = [0u8; 16];
        let mut iv = [0u8; 16];
        key.copy_from_slice(&s[0..16]);
        iv.copy_from_slice(&s[12..28]);

        let encoded = local_pk.to_encoded_point(true);
        let mut pub_key = vec![0x1D];
        pub_key.extend_from_slice(encoded.as_bytes()); // [0x1D, 0x02|0x03, X(28)]

        Ok(EcdhCipher { key, iv, pub_key })
    }

    /// AES-128-ECB "XOR-CFB": C_i = AES_ECB(P_i XOR C_{i-1}), C_{-1} = iv.
    fn encrypt(&self, plain: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new_from_slice(&self.key).expect("aes key");
        // PKCS#7 pad
        let block_len = 16usize;
        let pad = block_len - (plain.len() % block_len);
        let mut data = plain.to_vec();
        data.extend(std::iter::repeat(pad as u8).take(pad));

        let mut prev = self.iv;
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            let mut tmp = [0u8; 16];
            for i in 0..16 {
                tmp[i] = chunk[i] ^ prev[i];
            }
            let mut block = GenericArray::clone_from_slice(&tmp);
            cipher.encrypt_block(&mut block);
            prev.copy_from_slice(&block);
            out.extend_from_slice(&prev);
        }
        out
    }

    /// AES-128-CBC decrypt, then LZ4-decompress the inner block.
    fn decrypt(&self, ct: &[u8]) -> Result<Vec<u8>, AppError> {
        let cipher = Aes128::new_from_slice(&self.key).expect("aes key");
        let ct = &ct[0..ct.len() - ct.len() % 16];
        let mut prev = self.iv;
        let mut plain = vec![0u8; ct.len()];
        for (i, c) in ct.chunks(16).enumerate() {
            let mut block = GenericArray::clone_from_slice(c);
            cipher.decrypt_block(&mut block);
            for j in 0..16 {
                plain[i * 16 + j] = block[j] ^ prev[j];
            }
            prev.copy_from_slice(c);
        }
        // The first two bytes are the LE length of the LZ4 block.
        if plain.len() < 2 {
            return Err(AppError::CloudDrive("115 闪传响应过短".into()));
        }
        let len = (plain[0] as usize) | ((plain[1] as usize) << 8);
        if 2 + len > plain.len() {
            return Err(AppError::CloudDrive("115 闪传响应 LZ4 长度异常".into()));
        }
        let mut buf = vec![0u8; 0x2000];
        let n = lz4_flex::block::decompress_into(&plain[2..2 + len], &mut buf)
            .map_err(|e| AppError::CloudDrive(format!("115 闪传响应解压失败: {e}")))?;
        buf.truncate(n);
        Ok(buf)
    }

    /// `k_ec` token embedding the current millis timestamp (SDK EncodeToken).
    fn encode_token(&self, millis: u64) -> String {
        let mut rng = rand::rngs::OsRng;
        let r1 = rng.next_u32() as u8;
        let r2 = rng.next_u32() as u8;
        let time = (millis as u32).to_be_bytes();
        let mut tmp: Vec<u8> = Vec::with_capacity(48);
        for i in 0..15 {
            tmp.push(self.pub_key[i] ^ r1);
        }
        tmp.push(r1);
        tmp.push(0x73 ^ r1);
        for _ in 0..3 {
            tmp.push(r1);
        }
        for i in 0..4 {
            tmp.push(r1 ^ time[3 - i]);
        }
        for i in 15..self.pub_key.len() {
            tmp.push(self.pub_key[i] ^ r2);
        }
        tmp.push(r2);
        tmp.push(0x01 ^ r2);
        for _ in 0..3 {
            tmp.push(r2);
        }
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(CRC_SALT.as_bytes());
        hasher.update(&tmp);
        let crc = hasher.finalize().to_be_bytes();
        for i in 0..4 {
            tmp.push(crc[3 - i]);
        }
        base64::engine::general_purpose::STANDARD.encode(&tmp)
    }
}

fn sha1_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    hex_lower(&h.finalize())
}

fn md5_hex(data: &[u8]) -> String {
    let mut h = md5::Md5::new();
    h.update(data);
    hex_lower(&h.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `sig` form field (SDK GenerateSignature).
fn generate_signature(user_id: i64, user_key: &str, file_id: &str, target: &str) -> String {
    let first = sha1_hex(format!("{user_id}{file_id}{target}0").as_bytes());
    let sig = sha1_hex(format!("{user_key}{first}000000").as_bytes());
    sig.to_uppercase()
}

/// `token` form field (SDK GenerateToken; the preID param is unused by 115).
fn generate_token(
    user_id: i64,
    file_id: &str,
    file_size: &str,
    timestamp: &str,
    sign_key: &str,
    sign_val: &str,
) -> String {
    let uid = user_id.to_string();
    let uid_md5 = md5_hex(uid.as_bytes());
    let s = format!(
        "{MD5_SALT}{file_id}{file_size}{sign_key}{sign_val}{uid}{timestamp}{uid_md5}{APP_VERSION}"
    );
    md5_hex(s.as_bytes())
}

/// Outcome of a single ApiUploadInit submission.
#[derive(Debug, Clone)]
pub enum RapidUploadOutcome {
    /// status==2: rapid upload hit, file created in the target dir.
    Hit(String),
    /// status==7: 115 matched the SHA1 but requires proof we own the data —
    /// re-submit with `sign_key` plus `sign_val` = SHA1 of the `sign_check`
    /// byte range of the real file content.
    SignRequired {
        sign_key: String,
        sign_check: String,
    },
    /// status 1/4/…: not in the library / rejected; real upload required.
    Miss { status: i64, message: String },
}

/// Submit a SHA1-based rapid upload (first attempt, no sign proof).
/// Returns `SignRequired` when 115 wants the caller to prove file ownership.
pub async fn rapid_upload(
    cookie: &str,
    ua: &str,
    user_id: i64,
    user_key: &str,
    dir_id: &str,
    file_name: &str,
    file_size: u64,
    file_id: &str,
) -> Result<RapidUploadOutcome, AppError> {
    rapid_upload_signed(cookie, ua, user_id, user_key, dir_id, file_name, file_size, file_id, "", "")
        .await
}

fn sha1_upper_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02X}")).collect()
}

/// Full rapid-upload flow with sign proof: first attempt; if 115 returns
/// status==7 (`SignRequired`), reads the `sign_check` byte range of the real
/// file via `range_reader` (e.g. an HTTP Range request against the source
/// cloud file), computes `sign_val`, and re-submits. Returns the pickcode on
/// a hit.
pub async fn rapid_upload_with_range<F, Fut>(
    cookie: &str,
    ua: &str,
    user_id: i64,
    user_key: &str,
    dir_id: &str,
    file_name: &str,
    file_size: u64,
    file_id: &str,
    range_reader: F,
) -> Result<String, AppError>
where
    F: Fn(&str) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, AppError>>,
{
    let mut outcome = rapid_upload(cookie, ua, user_id, user_key, dir_id, file_name, file_size, file_id)
        .await?;
    loop {
        match outcome {
            RapidUploadOutcome::Hit(pickcode) => return Ok(pickcode),
            RapidUploadOutcome::SignRequired { sign_key, sign_check } => {
                let data = range_reader(&sign_check).await?;
                let sign_val = sha1_upper_hex(&data);
                outcome = rapid_upload_signed(
                    cookie, ua, user_id, user_key, dir_id, file_name, file_size, file_id,
                    &sign_key, &sign_val,
                )
                .await?;
            }
            RapidUploadOutcome::Miss { status, message } => {
                return Err(AppError::CloudDrive(format!(
                    "115 库中不存在该文件（闪传未命中，status={status}：{message}），无法秒传；真实上传暂未支持：{file_name}"
                )));
            }
        }
    }
}

/// Submit a SHA1-based rapid upload with an optional sign proof
/// (`sign_key`/`sign_val` from a previous `SignRequired` response).
pub async fn rapid_upload_signed(
    cookie: &str,
    ua: &str,
    user_id: i64,
    user_key: &str,
    dir_id: &str,
    file_name: &str,
    file_size: u64,
    file_id: &str,
    sign_key: &str,
    sign_val: &str,
) -> Result<RapidUploadOutcome, AppError> {
    let cipher = EcdhCipher::new()?;
    let target = format!("U_1_{}", dir_id.trim());
    let file_size_str = file_size.to_string();
    let uid = user_id.to_string();
    // 115 秒传的 QuickID 必须是大写 SHA1（SDK: strings.ToUpper(...)）；
    // 阿里云盘/分享返回的 sha1 是小写，不转大写会永远未命中（status 1/4）。
    let file_id = file_id.trim().to_uppercase();

    // base form (SDK sets these once)
    let mut form_pairs: Vec<(&str, String)> = vec![
        ("appid", "0".to_string()),
        ("appversion", APP_VERSION.to_string()),
        ("userid", uid.clone()),
        ("filename", file_name.to_string()),
        ("filesize", file_size_str.clone()),
        ("fileid", file_id.clone()),
        ("target", target.clone()),
        ("sig", generate_signature(user_id, user_key, &file_id, &target)),
        ("topupload", "true".to_string()),
    ];
    if !sign_key.is_empty() && !sign_val.is_empty() {
        form_pairs.push(("sign_key", sign_key.to_string()));
        form_pairs.push(("sign_val", sign_val.to_string()));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let t = now.to_string();
    let k_ec = cipher.encode_token(now);
    let token = generate_token(user_id, &file_id, &file_size_str, &t, sign_key, sign_val);
    form_pairs.push(("t", t));
    form_pairs.push(("token", token));

    // Scope the Serializer (borrows the String, not Send) so it drops before
    // the first await below — otherwise the whole future is not Send.
    // 115 服务端按 Go `url.Values.Encode()` 的字典序解析加密 payload，
    // 必须与上游（115driver/alist）编码顺序一致，否则签名回合会失败。
    form_pairs.sort_by(|a, b| a.0.cmp(b.0));
    let encoded_form = {
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &form_pairs {
            body.append_pair(k, v);
        }
        body.finish()
    };
    let encrypted = cipher.encrypt(encoded_form.as_bytes());

    let client = reqwest::Client::builder()
        .user_agent(ua)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))?;

    let resp = client
        .post(API_UPLOAD_INIT)
        .query(&[("k_ec", k_ec.as_str())])
        .header("Cookie", cookie)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encrypted)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求 115 闪传失败: {e}")))?;
    let status_code = resp.status();
    let body_bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取 115 闪传响应失败: {e}")))?;
    if !status_code.is_success() {
        return Err(AppError::CloudDrive(format!(
            "115 闪传 HTTP {status_code}：{}",
            String::from_utf8_lossy(&body_bytes)
        )));
    }
    let plain = cipher.decrypt(&body_bytes)?;
    let json: serde_json::Value = serde_json::from_slice(&plain)
        .map_err(|e| AppError::CloudDrive(format!("解析 115 闪传响应失败: {e}")))?;
    let status = json["status"].as_i64().unwrap_or(0);
    if status == 2 {
        let pickcode = json["pickcode"]
            .as_str()
            .ok_or_else(|| AppError::CloudDrive("115 闪传命中但缺少 pickcode".into()))?
            .to_string();
        return Ok(RapidUploadOutcome::Hit(pickcode));
    }
    if status == 7 {
        return Ok(RapidUploadOutcome::SignRequired {
            sign_key: json["sign_key"].as_str().unwrap_or("").to_string(),
            sign_check: json["sign_check"].as_str().unwrap_or("").to_string(),
        });
    }
    let msg = json["message"].as_str().unwrap_or("未知错误");
    Ok(RapidUploadOutcome::Miss {
        status,
        message: msg.to_string(),
    })
}

/// 115 目录列表接口（webapi.115.com/files，带 `sha1=1` 时每项含 `sha`
///（大写 SHA1）与 `pc`（pickcode））。
const API_DIR_FILES: &str = "https://webapi.115.com/files";

/// Look up a pickcode in a 115 directory by full SHA1 (`sha` field). Returns
/// `Ok(None)` when no file in the directory carries that SHA1. Used as a
/// fallback when rapid upload is rejected with status==4 (the file already
/// exists in the target dir, so 115 refuses to create a duplicate — the file
/// is right there and its pickcode gives us the CDN link directly).
pub async fn find_pickcode_by_sha1(
    cookie: &str,
    ua: &str,
    dir_id: &str,
    sha1: &str,
) -> Result<Option<String>, AppError> {
    let target = sha1.trim().to_uppercase();
    if target.is_empty() {
        return Ok(None);
    }
    let client = reqwest::Client::builder()
        .user_agent(ua)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::CloudDrive(format!("无法创建 HTTP 客户端: {e}")))?;
    let mut offset = 0u64;
    const PAGE: u64 = 100;
    loop {
        let url = format!(
            "{API_DIR_FILES}?cid={}&limit={PAGE}&offset={offset}&o=user_ptime&asc=0&fc_mix=0&format=json&sha1=1",
            dir_id.trim()
        );
        let resp = client
            .get(&url)
            .header("Cookie", cookie)
            .send()
            .await
            .map_err(|e| AppError::CloudDrive(format!("列出 115 目录失败: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::CloudDrive(format!("读取 115 目录响应失败: {e}")))?;
        if !status.is_success() {
            return Err(AppError::CloudDrive(format!(
                "列出 115 目录失败（HTTP {status}）：{}",
                text.chars().take(200).collect::<String>()
            )));
        }
        let j: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| AppError::CloudDrive(format!("解析 115 目录响应失败: {e}")))?;
        let items = j["data"].as_array().cloned().unwrap_or_default();
        for item in &items {
            let sha = item["sha"].as_str().unwrap_or("");
            if sha.eq_ignore_ascii_case(&target) {
                if let Some(pc) = item["pc"].as_str().filter(|s| !s.is_empty()) {
                    return Ok(Some(pc.to_string()));
                }
                // same SHA1 but no pickcode (folder item / unavailable) — skip
            }
        }
        if (items.len() as u64) < PAGE {
            break;
        }
        offset += PAGE;
        if offset > 5000 {
            break;
        }
    }
    Ok(None)
}

/// Fetch the 115 upload userkey for this cookie via app/uploadinfo.
///
/// The userkey participates in the rapid-upload `sig` (first round) AND is
/// validated by 115 in the sign-proof second round: with an empty userkey the
/// second submit is rejected with status=4, so every flash transfer silently
/// falls back to the directory lookup (which only works when the target dir
/// already holds the same SHA1). Fetching the real userkey makes the transfer
/// actually hit (status=7 -> sign proof -> status=2).
pub async fn fetch_upload_userkey(cookie: &str, ua: &str) -> Result<String, AppError> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://proapi.115.com/app/uploadinfo")
        .header("Cookie", cookie)
        .header("User-Agent", ua)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求 115 uploadinfo 失败: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取 115 uploadinfo 失败: {e}")))?;
    if !status.is_success() {
        let body = text.chars().take(300).collect::<String>();
        return Err(AppError::CloudDrive(format!(
            "请求 115 uploadinfo 失败（HTTP {status}）：{body}"
        )));
    }
    let j: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析 115 uploadinfo 失败: {e}")))?;
    let uk = j["userkey"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::CloudDrive("115 uploadinfo 响应缺少 userkey（cookie 可能已失效）".into())
        })?
        .to_string();
    Ok(uk)
}

/// Dynamically resolve the 115 最近接收 directory cid under the drive root;
/// create it when missing. Used as a fallback when a flash transfer with the
/// cached cid fails (directory deleted, or the cid differs per account).
pub async fn resolve_recent_recv_cid(cookie: &str, ua: &str) -> Result<String, AppError> {
    const RECENT_NAME: &str = "最近接收";
    // 1) list the drive root and look for 最近接收
    let url = format!(
        "{API_DIR_FILES}?cid=0&limit=56&offset=0&o=user_ptime&asc=0&fc_mix=0&show_dir=1&format=json&aid=1"
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Cookie", cookie)
        .header("User-Agent", ua)
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("请求 115 根目录失败: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取 115 根目录失败: {e}")))?;
    if !status.is_success() {
        let body = text.chars().take(300).collect::<String>();
        return Err(AppError::CloudDrive(format!(
            "请求 115 根目录失败（HTTP {status}）：{body}"
        )));
    }
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析 115 根目录失败: {e}")))?;
    for item in json["data"].as_array().cloned().unwrap_or_default() {
        if item["n"].as_str() == Some(RECENT_NAME) {
            if let Some(cid) = item["cid"].as_str() {
                return Ok(cid.to_string());
            }
        }
    }
    // 2) missing → create under the drive root
    let resp = client
        .post("https://webapi.115.com/files/add")
        .header("Cookie", cookie)
        .header("User-Agent", ua)
        .form(&[("pid", "0"), ("cname", RECENT_NAME)])
        .send()
        .await
        .map_err(|e| AppError::CloudDrive(format!("创建 115 最近接收目录失败: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::CloudDrive(format!("读取创建 115 目录响应失败: {e}")))?;
    if !status.is_success() {
        let body = text.chars().take(300).collect::<String>();
        return Err(AppError::CloudDrive(format!(
            "创建 115 最近接收目录失败（HTTP {status}）：{body}"
        )));
    }
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AppError::CloudDrive(format!("解析创建 115 目录响应失败: {e}")))?;
    let cid = json["data"]["file_id"]
        .as_str()
        .or_else(|| json["file_id"].as_str())
        .ok_or_else(|| {
            AppError::CloudDrive(format!(
                "创建 115 最近接收目录响应缺少 file_id: {}",
                text.chars().take(300).collect::<String>()
            ))
        })?
        .to_string();
    Ok(cid)
}

/// Rapid upload with sign proof, falling back to a SHA1 directory lookup when
/// 115 rejects the rapid upload (status==4 — the file already exists in the
/// target dir). Returns a pickcode either way.
pub async fn rapid_or_find_pickcode<F, Fut>(
    cookie: &str,
    ua: &str,
    user_id: i64,
    user_key: &str,
    dir_id: &str,
    file_name: &str,
    file_size: u64,
    file_id: &str,
    range_reader: F,
) -> Result<String, AppError>
where
    F: Fn(&str) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, AppError>>,
{
    match rapid_upload_with_range(
        cookie, ua, user_id, user_key, dir_id, file_name, file_size, file_id, range_reader,
    )
    .await
    {
        Ok(pickcode) => Ok(pickcode),
        Err(e) => {
            // status=4: 同目录已有同 SHA1 → 直接列出目标目录按 SHA1 找 pickcode。
            if let Some(pc) = find_pickcode_by_sha1(cookie, ua, dir_id, file_id).await? {
                return Ok(pc);
            }
            Err(e)
        }
    }
}
