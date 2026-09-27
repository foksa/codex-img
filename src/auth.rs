//! Reads the ChatGPT login that the Codex CLI keeps in $CODEX_HOME/auth.json.
//! Tokens are never refreshed here: refresh tokens rotate, so refreshing outside
//! codex could log codex out. Expired logins are left to codex to renew.
use crate::error::{Error, Result, RENEW_HINT};
use crate::util;
use base64::Engine;
use serde_json::Value;
use std::path::{Path, PathBuf};

const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const EXPIRY_MARGIN_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub access_token: String,
    pub account_id: String,
}

pub struct LoginStatus {
    pub auth_path: PathBuf,
    pub account_id: String,
    pub expires_at: Option<String>,
}

pub fn codex_home() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default();
    PathBuf::from(home).join(".codex")
}

pub fn auth_path() -> PathBuf {
    codex_home().join("auth.json")
}

pub fn decode_jwt_payload(token: &str) -> Result<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || parts[1].is_empty() {
        return Err(Error::auth("Codex access token is not a JWT. Run `codex login` again."));
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| Error::auth("Failed to decode Codex access token. Run `codex login` again."))
}

pub fn extract_account_id(token: &str) -> Result<String> {
    decode_jwt_payload(token)?[JWT_CLAIM_PATH]["chatgpt_account_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::auth("Codex access token does not contain chatgpt_account_id. Run `codex login` again."))
}

fn token_exp(token: &str) -> Result<Option<i64>> {
    Ok(decode_jwt_payload(token)?["exp"].as_f64().map(|exp| exp as i64))
}

pub fn token_expires_soon(token: &str, now: i64) -> Result<bool> {
    Ok(token_exp(token)?.is_none_or(|exp| exp - now < EXPIRY_MARGIN_SECS))
}

fn read_auth_file(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path).map_err(|_| {
        Error::auth(format!("No Codex login found at {}. Run `codex login` and sign in with ChatGPT.", path.display()))
    })?;
    serde_json::from_str(&raw)
        .map_err(|_| Error::auth(format!("Codex auth file {} is not valid JSON. Run `codex login` again.", path.display())))
}

fn credentials_from(auth: &Value) -> Result<Credentials> {
    let Some(access_token) = auth["tokens"]["access_token"].as_str().filter(|t| !t.is_empty()) else {
        let hint = if auth["OPENAI_API_KEY"].is_string() {
            " Codex is logged in with an API key; image generation here needs a ChatGPT login."
        } else {
            ""
        };
        return Err(Error::auth(format!(
            "Codex login has no ChatGPT access token.{hint} Run `codex login` and sign in with ChatGPT."
        )));
    };
    let account_id = match auth["tokens"]["account_id"].as_str().filter(|id| !id.is_empty()) {
        Some(id) => id.to_string(),
        None => extract_account_id(access_token)?,
    };
    Ok(Credentials { access_token: access_token.to_string(), account_id })
}

pub fn load_credentials_from(path: &Path, now: i64) -> Result<Credentials> {
    let credentials = credentials_from(&read_auth_file(path)?)?;
    if token_expires_soon(&credentials.access_token, now)? {
        return Err(Error::auth(format!("Your Codex login has expired. {RENEW_HINT}")));
    }
    Ok(credentials)
}

pub fn load_credentials() -> Result<Credentials> {
    load_credentials_from(&auth_path(), util::now_secs())
}

/// Offline login check: validates auth.json and the token's expiry without calling the backend.
pub fn login_status() -> Result<LoginStatus> {
    let auth_path = auth_path();
    let credentials = load_credentials_from(&auth_path, util::now_secs())?;
    let expires_at = token_exp(&credentials.access_token)?.map(util::iso8601);
    Ok(LoginStatus { auth_path, account_id: credentials.account_id, expires_at })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::Kind;
    use serde_json::json;

    pub fn jwt(payload: Value) -> String {
        let part = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
        format!("{}.{}.sig", part(json!({"alg": "none"})), part(payload))
    }

    pub fn token(exp_in: i64, account: &str) -> String {
        jwt(json!({"exp": util::now_secs() + exp_in, JWT_CLAIM_PATH: {"chatgpt_account_id": account}}))
    }

    pub fn write_auth(dir: &Path, access_token: &str) -> PathBuf {
        let path = dir.join("auth.json");
        let auth = json!({
            "auth_mode": "chatgpt", "OPENAI_API_KEY": null, "last_refresh": "2026-01-01T00:00:00Z",
            "tokens": {"id_token": "id", "access_token": access_token, "refresh_token": "rt", "account_id": "acct_123"},
        });
        std::fs::write(&path, auth.to_string()).unwrap();
        path
    }

    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("codex-img-test-{name}-{}", util::random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_account_id_and_expiry_from_claims() {
        assert_eq!(extract_account_id(&token(3600, "acct_x")).unwrap(), "acct_x");
        assert!(token_expires_soon(&token(30, "a"), util::now_secs()).unwrap());
        assert!(!token_expires_soon(&token(3600, "a"), util::now_secs()).unwrap());
        assert_eq!(decode_jwt_payload("nope").unwrap_err().kind, Kind::Auth);
    }

    #[test]
    fn missing_auth_file_is_an_auth_error() {
        let dir = temp_dir("missing");
        let error = load_credentials_from(&dir.join("auth.json"), util::now_secs()).unwrap_err();
        assert_eq!(error.kind, Kind::Auth);
        assert!(error.message.contains("codex login"));
    }

    #[test]
    fn expired_token_fails_with_renew_hint_and_leaves_file_untouched() {
        let dir = temp_dir("expired");
        let path = write_auth(&dir, &token(10, "acct_123"));
        let before = std::fs::read(&path).unwrap();
        let error = load_credentials_from(&path, util::now_secs()).unwrap_err();
        assert_eq!(error.kind, Kind::Auth);
        assert!(error.message.contains("Open Codex"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn valid_login_loads_credentials() {
        let dir = temp_dir("valid");
        let access = token(3600, "acct_claim");
        let path = write_auth(&dir, &access);
        let credentials = load_credentials_from(&path, util::now_secs()).unwrap();
        assert_eq!(credentials, Credentials { access_token: access, account_id: "acct_123".into() });
    }

    #[test]
    fn api_key_login_gets_a_specific_hint() {
        let error = credentials_from(&json!({"OPENAI_API_KEY": "sk-x", "tokens": null})).unwrap_err();
        assert!(error.message.contains("API key"));
    }
}
