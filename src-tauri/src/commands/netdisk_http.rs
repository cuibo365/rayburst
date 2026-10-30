//! Debug tracing for cloud-drive HTTP traffic.
//!
//! Every cloud-drive response funnels through this module, so raising the app
//! log level to `debug` reveals what each provider actually returned. Records
//! use the `netdisk` target, bodies are truncated, and credential-looking
//! values are masked, so debug logs stay safe to paste into a bug report.

use std::fmt;

use serde::de::DeserializeOwned;

/// Characters of a response body kept in the log. Providers can return
/// multi-megabyte listings; the head diagnoses nearly every failure.
const MAX_BODY_CHARS: usize = 8 * 1024;

/// Bytes previewed when a response body is binary (range requests, encrypted
/// payloads) — hex is only there to tell an empty body from a non-empty one.
const MAX_BINARY_PREVIEW: usize = 64;

/// JSON object keys (and URL query parameters) whose values are masked before
/// logging.  Protocol signatures that merely *prove* a request (115 `sign_key`,
/// `sign_check`) are deliberately excluded — they expire with the request and
/// are what makes a failed 闪传 diagnosable.
const SENSITIVE_KEY_FRAGMENTS: [&str; 7] = [
    "token",
    "cookie",
    "secret",
    "password",
    "authorization",
    "credential",
    "session",
];

/// Failure to obtain a decoded body: transport vs. payload shape.
#[derive(Debug)]
pub(crate) enum BodyError {
    Transport(reqwest::Error),
    Decode(serde_json::Error),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BodyError::Transport(e) => write!(f, "{e}"),
            BodyError::Decode(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BodyError::Transport(e) => Some(e),
            BodyError::Decode(e) => Some(e),
        }
    }
}

/// Logs status, URL and payload metadata without consuming the body.
///
/// Used for responses whose body is not read (header-only handshakes) and as
/// the first line `read_*` emit.
pub(crate) fn log_meta(resp: &reqwest::Response, label: &str) {
    if !crate::log_policy::debug_enabled() {
        return;
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    log::debug!(
        target: "netdisk",
        "[{label}] <- {} {content_type} len={:?} {}",
        resp.status().as_u16(),
        resp.content_length(),
        mask_url(resp.url())
    );
}

/// Reads a response as text, logging the (masked, truncated) body.
pub(crate) async fn read_text(
    resp: reqwest::Response,
    label: &str,
) -> Result<String, reqwest::Error> {
    log_meta(&resp, label);
    let text = resp.text().await?;
    log_body(label, &text);
    Ok(text)
}

/// Reads and deserializes a response body, logging the raw text first so a
/// shape change stays diagnosable when deserialization fails.
pub(crate) async fn read_json<T: DeserializeOwned>(
    resp: reqwest::Response,
    label: &str,
) -> Result<T, BodyError> {
    let text = read_text(resp, label).await.map_err(BodyError::Transport)?;
    serde_json::from_str(&text).map_err(BodyError::Decode)
}

/// Reads a binary response body, logging only a short hex preview.
pub(crate) async fn read_bytes(
    resp: reqwest::Response,
    label: &str,
) -> Result<Vec<u8>, reqwest::Error> {
    log_meta(&resp, label);
    let bytes = resp.bytes().await?;
    if crate::log_policy::debug_enabled() {
        let preview: String = bytes
            .iter()
            .take(MAX_BINARY_PREVIEW)
            .map(|b| format!("{b:02x}"))
            .collect();
        log::debug!(
            target: "netdisk",
            "[{label}] <- {} binary bytes, head={preview}",
            bytes.len()
        );
    }
    Ok(bytes.to_vec())
}

/// Logs an already-decoded payload (e.g. a decrypted 115 response).
pub(crate) fn log_body(label: &str, body: &str) {
    if !crate::log_policy::debug_enabled() {
        return;
    }
    let masked = mask_json_secrets(body);
    let char_count = masked.chars().count();
    if char_count > MAX_BODY_CHARS {
        let head: String = masked.chars().take(MAX_BODY_CHARS).collect();
        log::debug!(
            target: "netdisk",
            "[{label}] <- body {char_count} chars (truncated to {MAX_BODY_CHARS}): {head}"
        );
    } else {
        log::debug!(target: "netdisk", "[{label}] <- body: {masked}");
    }
}

/// Masks values of credential-looking keys in a JSON body; non-JSON bodies are
/// returned unchanged.
fn mask_json_secrets(body: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    mask_value(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| body.to_string())
}

fn mask_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if !child.is_null() && is_sensitive_key(key) {
                    *child = serde_json::Value::String("<redacted>".to_string());
                } else {
                    mask_value(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(mask_value),
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

/// Query parameters that carry a short-lived credential: 115 signs the flash
/// upload with `k_ec`, signed CDN links use `sig`/`signature`/`sign`.
const CREDENTIAL_QUERY_FRAGMENTS: [&str; 2] = ["sig", "k_ec"];

fn is_credential_query_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    is_sensitive_key(key)
        || CREDENTIAL_QUERY_FRAGMENTS
            .iter()
            .any(|fragment| lower.contains(fragment))
}

/// Masks credential-looking query parameters; the address itself stays intact
/// so the log still shows which endpoint was called.
fn mask_url(url: &reqwest::Url) -> String {
    if !url.query_pairs().any(|(key, _)| is_credential_query_key(&key)) {
        return url.as_str().to_string();
    }
    let masked = url
        .query_pairs()
        .map(|(key, value)| {
            if is_credential_query_key(&key) {
                // `<redacted>` must not go through `Url::set_query`, which
                // percent-encodes the angle brackets and hides the marker.
                format!("{key}=<redacted>")
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    let base = url.as_str().split('?').next().unwrap_or_default();
    format!("{base}?{masked}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_nested_credentials_and_keeps_the_rest() {
        let body =
            r#"{"code":0,"data":{"access_token":"abc","name":"n","items":[{"cookie":"c1"}]}}"#;
        let masked = mask_json_secrets(body);
        assert!(masked.contains("<redacted>"));
        assert!(masked.contains("\"name\":\"n\""));
        assert!(masked.contains("\"code\":0"));
        assert!(!masked.contains("abc"));
        assert!(!masked.contains("c1"));
    }

    #[test]
    fn masks_credentials_in_query_strings() {
        let url = reqwest::Url::parse("https://uplb.115.com/4.0/initupload.php?k_ec=abc&client=web")
            .expect("valid url");
        let masked = mask_url(&url);
        assert!(masked.contains("client=web"));
        assert!(!masked.contains("abc"));
        assert!(masked.contains("<redacted>"));
    }

    #[test]
    fn keeps_credential_free_urls_verbatim() {
        let url = reqwest::Url::parse("https://api.alipan.com/adrive/v3/file/list?limit=200")
            .expect("valid url");
        assert_eq!(mask_url(&url), url.as_str());
    }

    #[test]
    fn leaves_non_json_bodies_untouched() {
        let html = "<html><body>rate limited</body></html>";
        assert_eq!(mask_json_secrets(html), html);
    }

    #[test]
    fn masks_only_non_null_values() {
        let masked = mask_json_secrets(r#"{"token":null}"#);
        assert!(masked.contains("null"));
    }
}
