//! HTTP Basic auth for control-plane endpoints.
//!
//! Credentials are loaded once at startup from an htpasswd(5) file
//! (bcrypt only — generate with `htpasswd -B`). Operators get the
//! familiar Apache tooling for managing user/password pairs and the
//! server never has to deal with plaintext passwords on disk.
//!
//! See `docs/design/controlplane.md` § 3.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use tracing::warn;

use crate::state::AppState;

/// Parsed htpasswd file. Maps username -> bcrypt hash.
#[derive(Debug, Clone)]
pub struct HtpasswdFile {
    entries: HashMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HtpasswdError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("line {line}: malformed entry (expected 'user:hash')")]
    Malformed { line: usize },
    #[error(
        "line {line}: hash for user '{user}' is not bcrypt; \
         only bcrypt is accepted — regenerate with `htpasswd -B`"
    )]
    NotBcrypt { line: usize, user: String },
    #[error("file is empty — no credentials loaded")]
    Empty,
}

impl HtpasswdFile {
    pub fn load(path: &Path) -> Result<Self, HtpasswdError> {
        let raw = fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self, HtpasswdError> {
        let mut entries = HashMap::new();
        for (idx, line) in raw.lines().enumerate() {
            let line_no = idx + 1;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (user, hash) = trimmed
                .split_once(':')
                .ok_or(HtpasswdError::Malformed { line: line_no })?;
            if user.is_empty() || hash.is_empty() {
                return Err(HtpasswdError::Malformed { line: line_no });
            }
            // bcrypt prefixes: $2$, $2a$, $2b$, $2x$, $2y$.
            if !(hash.starts_with("$2a$")
                || hash.starts_with("$2b$")
                || hash.starts_with("$2x$")
                || hash.starts_with("$2y$"))
            {
                return Err(HtpasswdError::NotBcrypt {
                    line: line_no,
                    user: user.to_string(),
                });
            }
            entries.insert(user.to_string(), hash.to_string());
        }
        if entries.is_empty() {
            return Err(HtpasswdError::Empty);
        }
        Ok(Self { entries })
    }

    /// Verify a username/password pair.
    pub fn verify(&self, user: &str, pass: &str) -> bool {
        match self.entries.get(user) {
            Some(hash) => bcrypt::verify(pass, hash).unwrap_or(false),
            None => false,
        }
    }

    pub fn user_count(&self) -> usize {
        self.entries.len()
    }
}

/// Decoded `Authorization: Basic ...` credentials.
struct BasicCreds {
    user: String,
    pass: String,
}

fn parse_basic_header(headers: &HeaderMap) -> Option<BasicCreds> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let b64 = raw
        .strip_prefix("Basic ")
        .or_else(|| raw.strip_prefix("basic "))?;
    let decoded = BASE64.decode(b64.trim()).ok()?;
    let s = String::from_utf8(decoded).ok()?;
    let (user, pass) = s.split_once(':')?;
    Some(BasicCreds {
        user: user.to_string(),
        pass: pass.to_string(),
    })
}

fn unauthorized(message: &'static str) -> Response {
    let mut resp = (StatusCode::UNAUTHORIZED, message).into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"overacp\""),
    );
    resp
}

/// Axum middleware that requires HTTP Basic auth against the
/// htpasswd file held on `AppState`. Returns 503 (with no
/// `WWW-Authenticate`) when no file is configured — a deliberately
/// loud failure mode rather than open-by-default.
pub async fn require_basic_auth(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(file) = state.basic_auth.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "control-plane disabled: set OVERACP_BASIC_AUTH_FILE to an htpasswd file",
        )
            .into_response();
    };

    let Some(creds) = parse_basic_header(req.headers()) else {
        return unauthorized("missing or malformed Authorization header");
    };

    if !file.verify(&creds.user, &creds.pass) {
        warn!(user = %creds.user, "control-plane auth rejected");
        return unauthorized("invalid credentials");
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file() -> String {
        // Generate at the lowest cost so the unit tests stay fast.
        let hash = bcrypt::hash("hunter2", 4).unwrap();
        format!("# a comment\n\nalice:{hash}\n")
    }

    #[test]
    fn parses_and_verifies() {
        let f = HtpasswdFile::parse(&sample_file()).unwrap();
        assert_eq!(f.user_count(), 1);
        assert!(f.verify("alice", "hunter2"));
        assert!(!f.verify("alice", "wrong"));
        assert!(!f.verify("bob", "hunter2"));
    }

    #[test]
    fn rejects_non_bcrypt() {
        let raw = "alice:{SHA}xxxx\n";
        let err = HtpasswdFile::parse(raw).unwrap_err();
        assert!(matches!(err, HtpasswdError::NotBcrypt { .. }));
    }

    #[test]
    fn rejects_empty_file() {
        let err = HtpasswdFile::parse("# only a comment\n").unwrap_err();
        assert!(matches!(err, HtpasswdError::Empty));
    }

    #[test]
    fn rejects_malformed_line() {
        let err = HtpasswdFile::parse("nope-no-colon\n").unwrap_err();
        assert!(matches!(err, HtpasswdError::Malformed { line: 1 }));
    }

    #[test]
    fn rejects_empty_user_or_hash() {
        let err = HtpasswdFile::parse(":onlyhash\n").unwrap_err();
        assert!(matches!(err, HtpasswdError::Malformed { line: 1 }));
        let err = HtpasswdFile::parse("onlyuser:\n").unwrap_err();
        assert!(matches!(err, HtpasswdError::Malformed { line: 1 }));
    }

    #[test]
    fn load_reads_from_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("overacp-htpasswd-test-{}.txt", std::process::id()));
        std::fs::write(&path, sample_file()).unwrap();
        let f = HtpasswdFile::load(&path).unwrap();
        assert!(f.verify("alice", "hunter2"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_propagates_io_error_for_missing_file() {
        let path = std::env::temp_dir().join("overacp-htpasswd-does-not-exist-xyz");
        let err = HtpasswdFile::load(&path).unwrap_err();
        assert!(matches!(err, HtpasswdError::Io(_)));
    }

    #[test]
    fn parses_basic_header_roundtrip() {
        let mut h = HeaderMap::new();
        let token = BASE64.encode("alice:hunter2");
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {token}")).unwrap(),
        );
        let creds = parse_basic_header(&h).unwrap();
        assert_eq!(creds.user, "alice");
        assert_eq!(creds.pass, "hunter2");
    }

    #[test]
    fn parse_basic_header_rejects_non_basic() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer xxx"),
        );
        assert!(parse_basic_header(&h).is_none());
    }
}
