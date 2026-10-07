//! `rec export --upload`: the VM, and the take folder with it, is gone when the session
//! ends, so each export is delivered somewhere that outlives it.

use crate::error::{RecError, Result};
use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use ureq::http::Uri;

pub const BLOB_TOKEN_VAR: &str = "BLOB_READ_WRITE_TOKEN";
/// Points `blob` uploads at another API base, for tests.
pub const BLOB_API_VAR: &str = "REC_BLOB_API_URL";
const BLOB_API: &str = "https://vercel.com/api/blob";
/// The `x-api-version` that @vercel/blob 2.8 sends.
const BLOB_API_VERSION: &str = "12";
const CONTENT_TYPE: &str = "video/mp4";

#[derive(Debug, PartialEq)]
pub enum Target {
    /// Vercel Blob, through the same single PUT that @vercel/blob's `put()` makes.
    Blob {
        token: String,
        api: String,
        access: Access,
    },
    /// One presigned PUT URL, as S3, R2, GCS, and Mux direct uploads hand out.
    Presigned { url: String, public: String },
}

/// Must match the store's own setting; the Blob API rejects a mismatch.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Access {
    Public,
    Private,
}

impl Access {
    fn header(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Private => "private",
        }
    }
}

/// One export as `rec export` prints it, with `url` once it is uploaded.
#[derive(Serialize)]
pub struct Delivered<E> {
    #[serde(flatten)]
    pub export: E,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Target {
    pub fn parse(target: &str, env: impl Fn(&str) -> Option<String>) -> Result<Target> {
        let access = match target {
            "blob" => Some(Access::Public),
            "blob:private" => Some(Access::Private),
            _ => None,
        };
        if let Some(access) = access {
            let token = env(BLOB_TOKEN_VAR).filter(|t| !t.trim().is_empty()).ok_or_else(|| {
                RecError::new(
                    "missing_credentials",
                    format!("--upload blob needs {BLOB_TOKEN_VAR} set to a Vercel Blob read-write token"),
                )
            })?;
            let api = env(BLOB_API_VAR).unwrap_or_else(|| BLOB_API.to_string());
            return Ok(Target::Blob {
                token: token.trim().to_string(),
                api: api.trim_end_matches('/').to_string(),
                access,
            });
        }
        let bad = || {
            RecError::new(
                "bad_args",
                format!("unknown upload target {target:?}; use blob, blob:private, or a presigned https:// URL"),
            )
        };
        let uri: Uri = target.parse().map_err(|_| bad())?;
        let host = uri.host().ok_or_else(bad)?;
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
        match uri.scheme_str() {
            Some("https") => {}
            Some("http") if loopback => {}
            _ => return Err(bad()),
        }
        let authority = uri.authority().ok_or_else(bad)?;
        let public = format!(
            "{}://{authority}{}",
            uri.scheme_str().unwrap_or_default(),
            uri.path()
        );
        Ok(Target::Presigned {
            url: target.to_string(),
            public,
        })
    }

    /// A presigned URL names one object, so it can take only one export.
    pub fn check_layouts(&self, count: usize) -> Result<()> {
        match self {
            Target::Presigned { .. } if count != 1 => Err(RecError::new(
                "bad_args",
                format!("a presigned URL takes exactly one export, but {count} layouts were requested; pass one --layout"),
            )),
            _ => Ok(()),
        }
    }

    /// Streams `file` up and returns where it can be fetched.
    pub fn upload(&self, take_id: &str, file: &Path) -> Result<String> {
        let body =
            std::fs::File::open(file).map_err(|e| RecError::io(&file.display().to_string(), e))?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        match self {
            Target::Blob { token, api, access } => {
                let name = file.file_name().unwrap_or_default().to_string_lossy();
                let pathname = format!("rec/{take_id}/{name}");
                let store = token.split('_').nth(3).unwrap_or_default();
                let mut resp = agent
                    .put(format!("{api}/?pathname={}", form_encode(&pathname)))
                    .header("authorization", format!("Bearer {token}"))
                    .header("x-api-version", BLOB_API_VERSION)
                    .header("x-api-blob-request-id", request_id(store))
                    .header("x-api-blob-request-attempt", "0")
                    .header("x-vercel-blob-store-id", store)
                    .header("x-vercel-blob-access", access.header())
                    .header("x-content-type", CONTENT_TYPE)
                    .header("x-add-random-suffix", "0")
                    // Re-exporting the same take replaces the file at the same URL.
                    .header("x-allow-overwrite", "1")
                    .send(body)
                    .map_err(|e| failed(api, e))?;
                let text = checked(api, &mut resp)?;
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("url")?.as_str().map(str::to_string))
                    .ok_or_else(|| {
                        RecError::new(
                            "upload_failed",
                            format!("Vercel Blob reply has no url: {}", snippet(&text)),
                        )
                    })
            }
            Target::Presigned { url, public } => {
                let mut resp = agent
                    .put(url)
                    .header("content-type", CONTENT_TYPE)
                    .send(body)
                    .map_err(|e| failed(public, e))?;
                checked(public, &mut resp)?;
                Ok(public.clone())
            }
        }
    }
}

/// The body of a 2xx reply, or `upload_failed` with the status and the start of the body.
fn checked(dest: &str, resp: &mut ureq::http::Response<ureq::Body>) -> Result<String> {
    let mut text = String::new();
    let _ = resp
        .body_mut()
        .as_reader()
        .take(64 * 1024)
        .read_to_string(&mut text);
    let status = resp.status();
    if status.is_success() {
        return Ok(text);
    }
    Err(RecError::new(
        "upload_failed",
        format!(
            "PUT {dest} returned HTTP {}: {}",
            status.as_u16(),
            snippet(&text)
        ),
    ))
}

fn failed(dest: &str, e: ureq::Error) -> RecError {
    RecError::new("upload_failed", format!("PUT {dest}: {e}"))
}

fn snippet(text: &str) -> String {
    let t = text.trim();
    match t.char_indices().nth(300) {
        Some((i, _)) => format!("{}...", &t[..i]),
        None => t.to_string(),
    }
}

/// `<store>:<ms>:<hex>`, the shape @vercel/blob uses to correlate retries.
fn request_id(store: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let noise = now.subsec_nanos() ^ std::process::id().rotate_left(16);
    format!("{store}:{}:{noise:x}", now.as_millis())
}

/// `URLSearchParams` encoding, which is what @vercel/blob puts in `?pathname=`.
fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn blob_reads_its_token_and_api_override_from_the_environment() {
        assert_eq!(
            Target::parse(
                "blob",
                env(&[(BLOB_TOKEN_VAR, "vercel_blob_rw_store1_secret")])
            )
            .unwrap(),
            Target::Blob {
                token: "vercel_blob_rw_store1_secret".into(),
                api: BLOB_API.into(),
                access: Access::Public,
            }
        );
        assert_eq!(
            Target::parse(
                "blob",
                env(&[
                    (BLOB_TOKEN_VAR, "t"),
                    (BLOB_API_VAR, "http://127.0.0.1:9/api/")
                ])
            )
            .unwrap(),
            Target::Blob {
                token: "t".into(),
                api: "http://127.0.0.1:9/api".into(),
                access: Access::Public,
            }
        );
        assert!(matches!(
            Target::parse("blob:private", env(&[(BLOB_TOKEN_VAR, "t")])).unwrap(),
            Target::Blob {
                access: Access::Private,
                ..
            }
        ));
    }

    #[test]
    fn blob_without_a_token_names_the_variable() {
        for vars in [&[][..], &[(BLOB_TOKEN_VAR, " ")][..]] {
            let e = Target::parse("blob", env(vars)).unwrap_err();
            assert_eq!(e.code, "missing_credentials");
            assert!(e.message.contains(BLOB_TOKEN_VAR), "{}", e.message);
        }
    }

    #[test]
    fn presigned_urls_report_without_their_query() {
        let url =
            "https://bucket.s3.amazonaws.com/takes/a.mp4?X-Amz-Signature=abc&X-Amz-Expires=600";
        assert_eq!(
            Target::parse(url, env(&[])).unwrap(),
            Target::Presigned {
                url: url.into(),
                public: "https://bucket.s3.amazonaws.com/takes/a.mp4".into()
            }
        );
        let local = Target::parse("http://127.0.0.1:8123/up/x.mp4?sig=1", env(&[])).unwrap();
        assert!(
            matches!(local, Target::Presigned { public, .. } if public == "http://127.0.0.1:8123/up/x.mp4")
        );
    }

    #[test]
    fn other_targets_are_bad_args() {
        for t in [
            "s3",
            "http://example.com/x.mp4",
            "ftp://host/x",
            "https://",
            "",
            "BLOB",
        ] {
            let e = Target::parse(t, env(&[(BLOB_TOKEN_VAR, "t")])).unwrap_err();
            assert_eq!(e.code, "bad_args", "{t:?}: {e}");
        }
    }

    #[test]
    fn presigned_takes_exactly_one_layout() {
        let presigned = Target::parse("https://h/x.mp4?s=1", env(&[])).unwrap();
        assert!(presigned.check_layouts(1).is_ok());
        assert_eq!(presigned.check_layouts(2).unwrap_err().code, "bad_args");
        let blob = Target::parse("blob", env(&[(BLOB_TOKEN_VAR, "t")])).unwrap();
        assert!(blob.check_layouts(2).is_ok());
    }

    #[test]
    fn pathname_is_form_encoded_like_url_search_params() {
        assert_eq!(
            form_encode("rec/take-20261004-064206/export-16x9.mp4"),
            "rec%2Ftake-20261004-064206%2Fexport-16x9.mp4"
        );
    }
}
