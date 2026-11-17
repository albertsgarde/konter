//! Enable Banking API client (docs/fetching.md). The only code that talks to the
//! network; `konter build` only ever reads files on disk.
//!
//! Secrets live outside the repository, in `~/.config/konter/` (or
//! `$KONTER_CONFIG_DIR`):
//! - `enable-banking.toml`: application ID, private key path, redirect URL
//! - `sessions.toml`: one session per linked bank (written by `konter link`)
//! - `pending-links.toml`: links begun with `konter link --start`, until
//!   `--finish` completes them

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const API: &str = "https://api.enablebanking.com";

/// The JWT's lifetime; Enable Banking accepts at most 24 hours.
const TOKEN_SECONDS: u64 = 3600;

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("KONTER_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(dir).join("konter"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/konter"))
}

/// `path` with a leading `~` as the home folder.
pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// Writes a file that holds a secret: readable only by its owner (on Unix)
/// from the moment it's made, and written whole and then renamed into place,
/// so it's never there half-written.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().context("no parent folder")?;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp"));
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        // One left behind may be open to others, and `mode` only applies to
        // a file that's made here.
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().with_context(|| format!("writing {}", path.display()))
}

/// What a TOML file in the config folder holds, or nothing if it isn't there.
fn load_or_default<T: Default + serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// `enable-banking.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub application_id: String,
    /// Path to the application's private key (`<application_id>.pem`).
    pub private_key: String,
    /// One of the redirect URLs registered with the application.
    pub redirect_url: String,
}

impl Settings {
    pub fn load() -> Result<Settings> {
        let path = config_dir()?.join("enable-banking.toml");
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!("reading {} (see docs/fetching.md for how to set up Enable Banking)", path.display())
        })?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// `sessions.toml`: bank name → session.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Sessions {
    #[serde(default)]
    pub banks: BTreeMap<String, Session>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Session {
    pub session_id: String,
    /// RFC 3339 timestamp; the bank's consent ends then.
    pub valid_until: String,
    pub accounts: Vec<SessionAccount>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionAccount {
    pub uid: String,
    pub iban: Option<String>,
}

impl Sessions {
    fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("sessions.toml"))
    }

    pub fn load() -> Result<Sessions> {
        load_or_default(&Sessions::path()?)
    }

    pub fn save(&self) -> Result<()> {
        write_private(&Sessions::path()?, toml::to_string_pretty(self)?.as_bytes())
    }
}

/// `pending-links.toml`: links started with `konter link --start` and not yet
/// finished, bank name → the state the redirect has to carry back.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PendingLinks {
    #[serde(default)]
    pub banks: BTreeMap<String, PendingLink>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PendingLink {
    pub state: String,
    /// RFC 3339; the consent asked for.
    pub valid_until: String,
}

impl PendingLinks {
    fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("pending-links.toml"))
    }

    pub fn load() -> Result<PendingLinks> {
        load_or_default(&PendingLinks::path()?)
    }

    pub fn save(&self) -> Result<()> {
        let path = PendingLinks::path()?;
        if self.banks.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            return Ok(());
        }
        write_private(&path, toml::to_string_pretty(self)?.as_bytes())
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock before 1970").as_secs()
}

/// A signed JWT for the API (RS256, `kid` = application ID).
pub fn token(application_id: &str, private_key_pem: &[u8], now: u64) -> Result<String> {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(application_id.to_string());
    header.typ = Some("JWT".into());
    let claims = json!({
        "iss": "enablebanking.com",
        "aud": "api.enablebanking.com",
        "iat": now,
        "exp": now + TOKEN_SECONDS,
    });
    let key = EncodingKey::from_rsa_pem(private_key_pem).context("reading the private key (PEM)")?;
    Ok(jsonwebtoken::encode(&header, &claims, &key)?)
}

/// An error response from the API.
#[derive(Debug)]
pub struct ApiError {
    pub path: String,
    pub status: u16,
    pub body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: HTTP {}: {}", self.path, self.status, self.body)
    }
}

impl std::error::Error for ApiError {}

pub struct Client {
    agent: ureq::Agent,
    token: String,
}

impl Client {
    pub fn new(settings: &Settings) -> Result<Client> {
        let key_path = expand_home(Path::new(&settings.private_key));
        let pem = std::fs::read(&key_path).with_context(|| format!("reading {}", key_path.display()))?;
        let token = token(&settings.application_id, &pem, now_secs())?;
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Ok(Client { agent, token })
    }

    fn handle(path: &str, mut response: ureq::http::Response<ureq::Body>) -> Result<Value> {
        let status = response.status();
        let body = response.body_mut().read_to_string().unwrap_or_default();
        if !status.is_success() {
            return Err(ApiError { path: path.to_string(), status: status.as_u16(), body }.into());
        }
        serde_json::from_str(&body).with_context(|| format!("{path}: response is not JSON"))
    }

    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let mut request =
            self.agent.get(format!("{API}{path}")).header("Authorization", format!("Bearer {}", self.token));
        for (k, v) in query {
            request = request.query(*k, v);
        }
        Client::handle(path, request.call()?)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let response = self
            .agent
            .post(format!("{API}{path}"))
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(body)?;
        Client::handle(path, response)
    }

    /// Banks Enable Banking supports in a country.
    pub fn aspsps(&self, country: &str) -> Result<Vec<Value>> {
        let v = self.get("/aspsps", &[("country", country.to_string()), ("psu_type", "personal".into())])?;
        Ok(v.get("aspsps").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Starts authorization; returns the URL to open in a browser.
    pub fn start_auth(
        &self,
        aspsp: &str,
        country: &str,
        valid_until: &str,
        state: &str,
        redirect: &str,
    ) -> Result<String> {
        let body = json!({
            "access": { "valid_until": valid_until },
            "aspsp": { "name": aspsp, "country": country },
            "state": state,
            "redirect_url": redirect,
            "psu_type": "personal",
        });
        let v = self.post("/auth", &body)?;
        v.get("url").and_then(Value::as_str).map(str::to_string).context("/auth: no url in response")
    }

    pub fn create_session(&self, code: &str) -> Result<Value> {
        self.post("/sessions", &json!({ "code": code }))
    }

    /// All pages of transactions from `date_from` on, as returned by the API,
    /// and the date actually used: if the bank doesn't go back that far, the
    /// earliest date it allows.
    pub fn transaction_pages(&self, uid: &str, date_from: &str) -> Result<(Vec<Value>, String)> {
        match self.transaction_pages_from(uid, date_from) {
            Err(e) => {
                let earliest = e
                    .downcast_ref::<ApiError>()
                    .filter(|a| a.status == 422)
                    .and_then(|a| serde_json::from_str::<Value>(&a.body).ok())
                    .filter(|b| b.get("error").and_then(Value::as_str) == Some("WRONG_TRANSACTIONS_PERIOD"))
                    .and_then(|b| b.pointer("/detail/date_from").and_then(Value::as_str).map(str::to_string));
                match earliest {
                    Some(d) if d.as_str() > date_from => {
                        eprintln!("  the bank only goes back to {d}; fetching from there");
                        Ok((self.transaction_pages_from(uid, &d)?, d))
                    }
                    _ => Err(e),
                }
            }
            Ok(pages) => Ok((pages, date_from.to_string())),
        }
    }

    fn transaction_pages_from(&self, uid: &str, date_from: &str) -> Result<Vec<Value>> {
        let path = format!("/accounts/{uid}/transactions");
        let mut pages = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut query = vec![("date_from", date_from.to_string())];
            if let Some(key) = &continuation {
                query.push(("continuation_key", key.clone()));
            }
            let page = self.get(&path, &query)?;
            continuation =
                page.get("continuation_key").and_then(Value::as_str).filter(|k| !k.is_empty()).map(str::to_string);
            pages.push(page);
            if continuation.is_none() {
                return Ok(pages);
            }
            if pages.len() > 1000 {
                bail!("{path}: more than 1000 pages; giving up");
            }
        }
    }

    pub fn balances(&self, uid: &str) -> Result<Value> {
        self.get(&format!("/accounts/{uid}/balances"), &[])
    }
}

/// Pulls the authorization code out of what the user pasted: either the code
/// itself or the whole URL they were redirected to.
pub fn parse_redirect(pasted: &str, expected_state: &str) -> Result<String> {
    let pasted = pasted.trim();
    let Some((_, query)) = pasted.split_once('?') else {
        if pasted.is_empty() {
            bail!("nothing pasted");
        }
        return Ok(pasted.to_string());
    };
    let params: BTreeMap<&str, &str> = query.split('&').filter_map(|kv| kv.split_once('=')).collect();
    if let Some(error) = params.get("error") {
        bail!("the bank returned an error: {error} {}", params.get("error_description").unwrap_or(&""));
    }
    if let Some(state) = params.get("state")
        && *state != expected_state
    {
        bail!("state in the redirect doesn't match; start over with `konter link`");
    }
    params.get("code").map(|c| c.to_string()).context("no code= in the URL")
}

/// Formats a unix time as RFC 3339 (UTC).
pub fn rfc3339(secs: u64) -> String {
    let time = i64::try_from(secs).ok().and_then(|secs| jiff::Timestamp::from_second(secs).ok());
    time.map(|t| t.to_string()).unwrap_or_default()
}

/// Drops `null` fields, recursively. The API lists every field and fills in
/// few, so this shrinks stored responses by about two thirds without losing
/// anything (a missing field and a null one mean the same).
#[expect(clippy::wildcard_enum_match_arm, reason = "the kinds of JSON are fixed, and the rest hold no fields")]
pub fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, strip_nulls(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

/// Serializes a fetch for `data/`: compact, but with one top-level field and
/// one transaction per line, so diffs and greps stay readable.
pub fn to_raw_json(doc: &Value) -> String {
    let compact = |v: &Value| serde_json::to_string(v).expect("JSON values serialize");
    let Some(map) = doc.as_object() else { return compact(doc) + "\n" };
    let mut fields = Vec::new();
    for (key, value) in map {
        let text = match (key.as_str(), value) {
            ("pages", Value::Array(pages)) => {
                let pages: Vec<String> = pages
                    .iter()
                    .map(|page| {
                        let mut page = page.clone();
                        let txs = page.as_object_mut().and_then(|p| p.remove("transactions"));
                        let rest = compact(&page);
                        let txs: Vec<String> = txs
                            .as_ref()
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .map(|t| format!("  {}", compact(t)))
                            .collect();
                        // {"continuation_key":…,"transactions":[\n tx,\n tx\n]}
                        let head = rest.trim_end_matches('}');
                        let sep = if head.len() > 1 { "," } else { "" };
                        format!("{head}{sep}\"transactions\":[\n{}\n]}}", txs.join(",\n"))
                    })
                    .collect();
                format!("[\n{}\n]", pages.join(",\n"))
            }
            _ => compact(value),
        };
        fields.push(format!("{}:{text}", compact(&Value::String(key.clone()))));
    }
    format!("{{\n{}\n}}\n", fields.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway 2048-bit test key, used only to check the JWT format.
    const TEST_KEY: &str = include_str!("../tests/fixtures/test-key.pem");
    const TEST_PUB: &str = include_str!("../tests/fixtures/test-key.pub.pem");

    #[test]
    fn signs_a_valid_jwt() {
        let token = token("app-id", TEST_KEY.as_bytes(), 1_800_000_000).expect("the token is signed");
        let header = jsonwebtoken::decode_header(&token).expect("the token has a header");
        assert_eq!(header.kid.as_deref(), Some("app-id"));
        assert_eq!(header.alg, Algorithm::RS256);
        let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
        validation.set_audience(&["api.enablebanking.com"]);
        validation.set_issuer(&["enablebanking.com"]);
        validation.validate_exp = false;
        let key = jsonwebtoken::DecodingKey::from_rsa_pem(TEST_PUB.as_bytes()).expect("the test's public key reads");
        let data =
            jsonwebtoken::decode::<Value>(&token, &key, &validation).expect("the token verifies with the public key");
        assert_eq!(data.claims["exp"], 1_800_000_000u64 + TOKEN_SECONDS);
    }

    #[test]
    fn secrets_are_written_for_the_owner_only() {
        let dir = std::env::temp_dir().join(format!("konter-write-private-{}", std::process::id()));
        let path = dir.join("nested/sessions.toml");
        write_private(&path, b"first").expect("the file is written");
        // Over what's there, and over a temporary file left behind.
        std::fs::write(dir.join("nested/.sessions.toml.tmp"), "left behind").expect("the leftover file is written");
        write_private(&path, b"second").expect("the file is written over");
        assert_eq!(std::fs::read_to_string(&path).expect("the file is read"), "second");
        assert!(!dir.join("nested/.sessions.toml.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).expect("the file has metadata").permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).expect("the scratch folder is removed");
    }

    #[test]
    fn a_leading_tilde_is_the_home_folder() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        assert_eq!(expand_home(Path::new("~/keys/app.pem")), home.join("keys/app.pem"));
        assert_eq!(expand_home(Path::new("~")), home);
        assert_eq!(expand_home(Path::new("/etc/~/x")), Path::new("/etc/~/x"));
        assert_eq!(expand_home(Path::new("~other/x")), Path::new("~other/x"));
    }

    #[test]
    fn redirect_parsing() {
        assert_eq!(
            parse_redirect("https://localhost/cb?state=s1&code=abc", "s1").expect("the redirect has a code"),
            "abc"
        );
        assert_eq!(parse_redirect("  abc  ", "s1").expect("a bare code is taken as it is"), "abc");
        assert!(parse_redirect("https://localhost/cb?state=other&code=abc", "s1").is_err());
        assert!(parse_redirect("https://localhost/cb?error=access_denied", "s1").is_err());
    }

    #[test]
    fn round_trips_without_nulls() {
        let doc = json!({
            "acct_fetch": 1,
            "pages": [
                {"continuation_key": "k", "transactions": [{"a": 1, "b": null, "c": {"d": null, "e": "x"}}, {"a": 2}]},
                {"transactions": []}
            ],
            "balances": {"balances": [{"x": null}]}
        });
        let text = to_raw_json(&strip_nulls(doc.clone()));
        let back: Value = serde_json::from_str(&text).expect("what's stored is JSON");
        assert_eq!(back, strip_nulls(doc));
        assert!(!text.contains("null"));
        assert!(text.contains("\n  {\"a\":2}\n"), "{text}");
    }
}
