use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Method, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};
use tokio::sync::Mutex;

const EMAIL_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'@')
    .remove(b'.')
    .remove(b'_')
    .remove(b'-');
const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Authentication material derived by the transport for one request. The
/// principal is authoritative; tool arguments never select credentials or a
/// domain-wide delegation subject.
#[derive(Clone, Debug)]
pub struct RequestAuthContext {
    pub principal_email: String,
    pub google_access_token: Option<String>,
    pub granted_scopes: Vec<String>,
}

tokio::task_local! {
    static REQUEST_AUTH: RequestAuthContext;
}

pub async fn with_request_auth<F, T>(context: RequestAuthContext, future: F) -> T
where
    F: Future<Output = T>,
{
    REQUEST_AUTH.scope(context, future).await
}

pub fn request_principal_email() -> Option<String> {
    REQUEST_AUTH
        .try_with(|context| context.principal_email.clone())
        .ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
    pub token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_uri: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub expiry: Option<String>,
}

#[derive(Clone)]
pub struct GoogleSession {
    client: reqwest::Client,
    access_token: String,
    pub user_email: String,
}

impl GoogleSession {
    pub async fn authenticate(required_scopes: &[&str]) -> Result<Self, String> {
        if let Ok(context) = REQUEST_AUTH.try_with(Clone::clone) {
            let principal = canonical_email(&context.principal_email)?;
            if let Some(access_token) = context.google_access_token {
                if !has_required_scopes(&context.granted_scopes, required_scopes) {
                    return Err(format!(
                        "Authentication required for {principal}. The verified access token does not include the required Google scopes."
                    ));
                }
                return Self::from_access_token(access_token, principal);
            }
            if service_account_configured() {
                let access_token =
                    service_account_access_token(&principal, required_scopes).await?;
                return Self::from_access_token(access_token, principal);
            }
            return Self::authenticate_local(&principal, required_scopes).await;
        }

        let user_email = canonical_email(&env::var("USER_GOOGLE_EMAIL").map_err(|_| {
            String::from(
                "Native Rust Google tools require USER_GOOGLE_EMAIL to pin the authenticated principal",
            )
        })?)?;
        if service_account_configured() {
            let access_token = service_account_access_token(&user_email, required_scopes).await?;
            return Self::from_access_token(access_token, user_email);
        }
        Self::authenticate_local(&user_email, required_scopes).await
    }

    async fn authenticate_local(
        user_email: &str,
        required_scopes: &[&str],
    ) -> Result<Self, String> {
        let store = LocalCredentialStore::from_env()?;
        let mut credential = store.load(user_email)?.ok_or_else(|| {
            format!(
                "Authentication required for {user_email}. No stored Google credentials were found. Run start_google_auth first."
            )
        })?;
        if !has_required_scopes(&credential.scopes, required_scopes) {
            return Err(format!(
                "Authentication required for {user_email}. Stored credentials do not include the required Google scopes."
            ));
        }

        if credential.token.as_deref().is_none_or(str::is_empty)
            || credential_is_expired(&credential)
        {
            refresh_credential(&mut credential).await?;
            store.store(user_email, &credential)?;
        }
        let access_token = credential.token.clone().ok_or_else(|| {
            format!("Authentication required for {user_email}. Stored access token is empty.")
        })?;
        Self::from_access_token(access_token, user_email.to_owned())
    }

    fn from_access_token(access_token: String, user_email: String) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| String::from("Failed to initialize Google API HTTP client"))?;
        Ok(Self {
            client,
            access_token,
            user_email,
        })
    }

    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client
            .request(method, url)
            .bearer_auth(&self.access_token)
    }

    pub async fn send_json(
        &self,
        tool_name: &str,
        request: RequestBuilder,
    ) -> Result<Value, String> {
        let response = request
            .send()
            .await
            .map_err(|_| format!("API error in {tool_name}: Google API request failed"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|_| format!("API error in {tool_name}: failed to read Google API response"))?;
        if !status.is_success() {
            return Err(google_api_error(tool_name, status, &body));
        }
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&body)
            .map_err(|_| format!("API error in {tool_name}: Google API returned invalid JSON"))
    }
}

#[derive(Debug, Clone)]
pub struct LocalCredentialStore {
    directory: PathBuf,
}

impl LocalCredentialStore {
    pub fn from_env() -> Result<Self, String> {
        let directory = env::var_os("WORKSPACE_MCP_CREDENTIALS_DIR")
            .or_else(|| env::var_os("GOOGLE_MCP_CREDENTIALS_DIR"))
            .map(expand_home)
            .or_else(|| home_directory().map(|home| home.join(".google_workspace_mcp/credentials")))
            .unwrap_or_else(|| PathBuf::from(".credentials"));
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => validate_credential_directory(&directory, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&directory).map_err(|error| {
                    format!(
                        "Failed to create credentials directory {}: {error}",
                        directory.display()
                    )
                })?;
                let metadata = fs::symlink_metadata(&directory).map_err(|error| {
                    format!(
                        "Failed to inspect credentials directory {}: {error}",
                        directory.display()
                    )
                })?;
                validate_credential_directory(&directory, &metadata)?;
            }
            Err(error) => {
                return Err(format!(
                    "Failed to inspect credentials directory {}: {error}",
                    directory.display()
                ));
            }
        }
        set_directory_permissions(&directory)?;
        let directory = directory.canonicalize().map_err(|error| {
            format!(
                "Failed to resolve credentials directory {}: {error}",
                directory.display()
            )
        })?;
        Ok(Self { directory })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn load(&self, user_email: &str) -> Result<Option<StoredCredential>, String> {
        let path = self.existing_path(user_email)?;
        let Some(path) = path else {
            return Ok(None);
        };
        let mut file = open_credential_file(&path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|error| format!("Failed to read credentials {}: {error}", path.display()))?;
        serde_json::from_str(&content)
            .map(Some)
            .map_err(|error| format!("Invalid credential JSON {}: {error}", path.display()))
    }

    pub fn store(&self, user_email: &str, credential: &StoredCredential) -> Result<(), String> {
        let target = self.canonical_path(user_email)?;
        validate_existing_credential_target(&target)?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| String::from("Invalid credential filename"))?;
        let temporary = self
            .directory
            .join(format!(".{file_name}.{}.{unique}.tmp", std::process::id()));
        let serialized = serde_json::to_vec_pretty(credential)
            .map_err(|error| format!("Failed to serialize credentials: {error}"))?;

        let result = (|| -> Result<(), String> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|error| {
                format!(
                    "Failed to create temporary credential file {}: {error}",
                    temporary.display()
                )
            })?;
            file.write_all(&serialized)
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.flush())
                .and_then(|()| file.sync_all())
                .map_err(|error| format!("Failed to persist credentials: {error}"))?;
            fs::rename(&temporary, &target).map_err(|error| {
                format!(
                    "Failed to replace credential file {}: {error}",
                    target.display()
                )
            })?;
            sync_directory(&self.directory)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn existing_path(&self, user_email: &str) -> Result<Option<PathBuf>, String> {
        let canonical = self.canonical_path(user_email)?;
        if credential_path_exists(&canonical)? {
            return Ok(Some(canonical));
        }
        let legacy_name: String = user_email
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || "@._-".contains(character) {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let legacy = self.directory.join(format!("{legacy_name}.json"));
        credential_path_exists(&legacy).map(|exists| exists.then_some(legacy))
    }

    fn canonical_path(&self, user_email: &str) -> Result<PathBuf, String> {
        let email = user_email.trim();
        if email.is_empty() {
            return Err(String::from("user_email must be a non-empty string"));
        }
        let encoded = utf8_percent_encode(email, EMAIL_ENCODE_SET).to_string();
        let path = self.directory.join(format!("{encoded}.json"));
        if path.parent() != Some(self.directory.as_path()) {
            return Err(format!("Invalid credential path: {}", path.display()));
        }
        Ok(path)
    }
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_seconds")]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

pub(crate) async fn refresh_credential(credential: &mut StoredCredential) -> Result<(), String> {
    let refresh_token = required_credential_field(&credential.refresh_token, "refresh_token")?;
    let token_uri = required_credential_field(&credential.token_uri, "token_uri")?;
    let client_id = required_credential_field(&credential.client_id, "client_id")?;
    let client_secret = credential
        .client_secret
        .clone()
        .ok_or_else(refresh_fields_error)?;
    let url = Url::parse(&token_uri).map_err(|_| String::from("Stored token_uri is invalid"))?;
    if url.scheme() != "https" || url.host_str() != Some("oauth2.googleapis.com") {
        return Err(String::from(
            "Stored token_uri must be https://oauth2.googleapis.com/token",
        ));
    }

    let mut form = vec![
        ("grant_type", String::from("refresh_token")),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
    ];
    if !credential.scopes.is_empty() {
        form.push(("scope", credential.scopes.join(" ")));
    }
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|_| String::from("Failed to initialize OAuth refresh client"))?;
    let response = client
        .post(url)
        .form(&form)
        .send()
        .await
        .map_err(|_| String::from("Google OAuth token refresh request failed"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| String::from("Failed to read Google OAuth refresh response"))?;
    if !status.is_success() {
        return Err(format!(
            "Google OAuth token refresh failed with status {status}. Re-authentication is required."
        ));
    }
    let refreshed: RefreshResponse = serde_json::from_str(&body)
        .map_err(|_| String::from("Google OAuth token refresh returned invalid JSON"))?;
    credential.token = Some(refreshed.access_token);
    if let Some(rotated) = refreshed.refresh_token {
        credential.refresh_token = Some(rotated);
    }
    if let Some(seconds) = refreshed.expires_in {
        let expiry = OffsetDateTime::now_utc() + time::Duration::seconds(seconds);
        credential.expiry = expiry.format(&Rfc3339).ok();
    }
    if let Some(scope) = refreshed.scope {
        credential.scopes = scope.split_whitespace().map(str::to_owned).collect();
    }
    Ok(())
}

pub(crate) fn credential_is_expired(credential: &StoredCredential) -> bool {
    let Some(expiry) = credential.expiry.as_deref() else {
        return false;
    };
    let parsed = OffsetDateTime::parse(expiry, &Rfc3339).ok().or_else(|| {
        let seconds = expiry.get(..19)?;
        PrimitiveDateTime::parse(
            seconds,
            format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]"),
        )
        .ok()
        .map(PrimitiveDateTime::assume_utc)
    });
    parsed
        .map(|expiry| {
            expiry
                <= OffsetDateTime::now_utc()
                    + time::Duration::seconds(REFRESH_MARGIN.as_secs() as i64)
        })
        .unwrap_or(false)
}

pub(crate) fn has_required_scopes(available: &[String], required: &[&str]) -> bool {
    let mut expanded: HashSet<&str> = available.iter().map(String::as_str).collect();
    for scope in available {
        for child in scope_children(scope) {
            expanded.insert(child);
        }
    }
    required.iter().all(|scope| expanded.contains(scope))
}

fn scope_children(scope: &str) -> &'static [&'static str] {
    match scope {
        "https://www.googleapis.com/auth/tasks" => {
            &["https://www.googleapis.com/auth/tasks.readonly"]
        }
        "https://www.googleapis.com/auth/forms.body" => {
            &["https://www.googleapis.com/auth/forms.body.readonly"]
        }
        "https://www.googleapis.com/auth/presentations" => {
            &["https://www.googleapis.com/auth/presentations.readonly"]
        }
        "https://www.googleapis.com/auth/calendar" => &[
            "https://www.googleapis.com/auth/calendar.readonly",
            "https://www.googleapis.com/auth/calendar.events",
        ],
        "https://www.googleapis.com/auth/contacts" => {
            &["https://www.googleapis.com/auth/contacts.readonly"]
        }
        "https://www.googleapis.com/auth/chat.messages" => {
            &["https://www.googleapis.com/auth/chat.messages.readonly"]
        }
        "https://www.googleapis.com/auth/chat.spaces" => {
            &["https://www.googleapis.com/auth/chat.spaces.readonly"]
        }
        "https://www.googleapis.com/auth/spreadsheets" => {
            &["https://www.googleapis.com/auth/spreadsheets.readonly"]
        }
        "https://www.googleapis.com/auth/documents" => {
            &["https://www.googleapis.com/auth/documents.readonly"]
        }
        "https://www.googleapis.com/auth/script.projects" => {
            &["https://www.googleapis.com/auth/script.projects.readonly"]
        }
        "https://www.googleapis.com/auth/script.deployments" => {
            &["https://www.googleapis.com/auth/script.deployments.readonly"]
        }
        "https://www.googleapis.com/auth/gmail.modify" => &[
            "https://www.googleapis.com/auth/gmail.readonly",
            "https://www.googleapis.com/auth/gmail.send",
            "https://www.googleapis.com/auth/gmail.compose",
            "https://www.googleapis.com/auth/gmail.labels",
        ],
        "https://www.googleapis.com/auth/drive" => &[
            "https://www.googleapis.com/auth/drive.readonly",
            "https://www.googleapis.com/auth/drive.file",
        ],
        _ => &[],
    }
}

fn google_api_error(tool_name: &str, status: StatusCode, body: &str) -> String {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return format!(
            "API error in {tool_name}: Google authorization failed with status {status}. Re-authentication may be required."
        );
    }
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from("Google API request failed"));
    format!("API error in {tool_name}: {status}: {message}")
}

fn required_credential_field(value: &Option<String>, _name: &str) -> Result<String, String> {
    value.clone().ok_or_else(refresh_fields_error)
}

fn refresh_fields_error() -> String {
    String::from(
        "The credentials do not contain the necessary fields need to refresh the access token. You must specify refresh_token, token_uri, client_id, and client_secret.",
    )
}

#[derive(Debug, Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    #[serde(default = "default_google_token_uri")]
    token_uri: String,
}

#[derive(Debug, Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: String,
    aud: &'a str,
    iat: i64,
    exp: i64,
    sub: &'a str,
}

#[derive(Clone)]
struct CachedServiceToken {
    token: String,
    expires_at: OffsetDateTime,
}

static SERVICE_TOKEN_CACHE: OnceLock<Arc<Mutex<HashMap<String, CachedServiceToken>>>> =
    OnceLock::new();

fn service_account_configured() -> bool {
    env::var_os("GOOGLE_SERVICE_ACCOUNT_KEY_FILE").is_some()
        || env::var_os("GOOGLE_SERVICE_ACCOUNT_KEY_JSON").is_some()
}

async fn service_account_access_token(
    subject: &str,
    required_scopes: &[&str],
) -> Result<String, String> {
    enforce_dwd_policy(subject)?;
    let key = read_service_account_key()?;
    let mut scopes = required_scopes.to_vec();
    scopes.sort_unstable();
    scopes.dedup();
    let cache_key = format!("{}|{}|{}", key.client_email, subject, scopes.join(" "));
    let cache = SERVICE_TOKEN_CACHE
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone();
    {
        let cached = cache.lock().await;
        if let Some(entry) = cached.get(&cache_key)
            && entry.expires_at > OffsetDateTime::now_utc() + time::Duration::minutes(5)
        {
            return Ok(entry.token.clone());
        }
    }

    let token_uri = Url::parse(&key.token_uri)
        .map_err(|_| String::from("Service account token_uri is invalid"))?;
    if token_uri.as_str() != GOOGLE_TOKEN_ENDPOINT {
        return Err(String::from(
            "Service account token_uri must be https://oauth2.googleapis.com/token",
        ));
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let claims = ServiceAccountClaims {
        iss: &key.client_email,
        scope: scopes.join(" "),
        aud: GOOGLE_TOKEN_ENDPOINT,
        iat: now,
        exp: now + 3_600,
        sub: subject,
    };
    let encoding_key = EncodingKey::from_rsa_pem(key.private_key.as_bytes())
        .map_err(|_| String::from("Service account private key is invalid"))?;
    let assertion = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key)
        .map_err(|_| String::from("Failed to sign service account assertion"))?;
    let client = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|_| String::from("Failed to initialize service account token client"))?;
    let response = client
        .post(GOOGLE_TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ])
        .send()
        .await
        .map_err(|_| String::from("Service account token request failed"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| String::from("Failed to read service account token response"))?;
    if !status.is_success() {
        return Err(format!(
            "Service account token request failed with status {status}"
        ));
    }
    let token: RefreshResponse = serde_json::from_str(&body)
        .map_err(|_| String::from("Service account token response was invalid"))?;
    let expires_in = token.expires_in.unwrap_or(3_600).clamp(1, 3_600);
    let expires_at = OffsetDateTime::now_utc() + time::Duration::seconds(expires_in);
    cache.lock().await.insert(
        cache_key,
        CachedServiceToken {
            token: token.access_token.clone(),
            expires_at,
        },
    );
    Ok(token.access_token)
}

fn read_service_account_key() -> Result<ServiceAccountKey, String> {
    let file = env::var_os("GOOGLE_SERVICE_ACCOUNT_KEY_FILE");
    let inline = env::var_os("GOOGLE_SERVICE_ACCOUNT_KEY_JSON");
    let content = match (file, inline) {
        (Some(_), Some(_)) => {
            return Err(String::from(
                "GOOGLE_SERVICE_ACCOUNT_KEY_FILE and GOOGLE_SERVICE_ACCOUNT_KEY_JSON are mutually exclusive",
            ));
        }
        (Some(path), None) => fs::read_to_string(PathBuf::from(path))
            .map_err(|_| String::from("Failed to read service account key file"))?,
        (None, Some(json)) => json
            .into_string()
            .map_err(|_| String::from("GOOGLE_SERVICE_ACCOUNT_KEY_JSON is not valid Unicode"))?,
        (None, None) => return Err(String::from("Service account key is not configured")),
    };
    serde_json::from_str(&content).map_err(|_| String::from("Service account key JSON is invalid"))
}

fn enforce_dwd_policy(subject: &str) -> Result<(), String> {
    let subject = canonical_email(subject)?;
    if let Ok(fixed) = env::var("USER_GOOGLE_EMAIL")
        && !fixed.trim().is_empty()
        && !fixed.eq_ignore_ascii_case(&subject)
    {
        return Err(String::from(
            "The verified principal does not match the fixed domain-wide delegation subject",
        ));
    }
    let domains = env::var("DWD_ALLOWED_DOMAINS")
        .ok()
        .map(|value| {
            value
                .split(|character: char| character == ',' || character.is_whitespace())
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let per_request = request_principal_email().is_some();
    if per_request && domains.is_empty() {
        return Err(String::from(
            "DWD_ALLOWED_DOMAINS is required for per-request domain-wide delegation",
        ));
    }
    if !domains.is_empty() {
        let domain = subject
            .rsplit_once('@')
            .map(|(_, domain)| domain)
            .ok_or_else(|| String::from("Invalid domain-wide delegation subject"))?;
        if !domains.contains(&domain.to_ascii_lowercase()) {
            return Err(String::from(
                "The domain-wide delegation subject is outside DWD_ALLOWED_DOMAINS",
            ));
        }
    }
    Ok(())
}

pub(crate) fn canonical_email(value: &str) -> Result<String, String> {
    let email = value.trim().to_ascii_lowercase();
    let valid = email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty()
            && !domain.is_empty()
            && domain.contains('.')
            && !email.chars().any(char::is_whitespace)
    });
    if valid {
        Ok(email)
    } else {
        Err(String::from("Authenticated principal email is invalid"))
    }
}

fn default_google_token_uri() -> String {
    String::from(GOOGLE_TOKEN_ENDPOINT)
}

fn deserialize_optional_seconds<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expires_in must be an integer")),
        Some(Value::String(value)) => value
            .parse::<i64>()
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(_) => Err(serde::de::Error::custom(
            "expires_in must be a number or numeric string",
        )),
    }
}

fn expand_home(path: std::ffi::OsString) -> PathBuf {
    let path = PathBuf::from(path);
    let Some(value) = path.to_str() else {
        return path;
    };
    if value == "~" {
        return home_directory().unwrap_or(path);
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = home_directory()
    {
        return home.join(rest);
    }
    path
}

fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn validate_credential_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Credentials directory must not be a symbolic link: {}",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "Credentials path is not a directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn credential_path_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_credential_file_type(path, &metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect credential file {}: {error}",
            path.display()
        )),
    }
}

fn validate_credential_file_type(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Credential file must not be a symbolic link: {}",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "Credential path is not a regular file: {}",
            path.display()
        ));
    }
    Ok(())
}

fn open_credential_file(path: &Path) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|error| {
        format!(
            "Failed to securely open credentials {}: {error}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Failed to inspect credentials {}: {error}", path.display()))?;
    validate_credential_file_type(path, &metadata)?;
    validate_unix_credential_metadata(path, &metadata)?;
    Ok(file)
}

fn validate_existing_credential_target(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_credential_file_type(path, &metadata)?;
            validate_unix_credential_metadata(path, &metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to inspect credential target {}: {error}",
            path.display()
        )),
    }
}

fn validate_unix_credential_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.uid() != effective_uid {
            return Err(format!(
                "Credential file is not owned by the current user: {}",
                path.display()
            ));
        }
        if metadata.mode() & 0o7777 != 0o600 {
            return Err(format!(
                "Credential file permissions must be 0600: {}",
                path.display()
            ));
        }
        if metadata.nlink() != 1 {
            return Err(format!(
                "Credential file must have exactly one hard link: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!(
                    "Failed to synchronize credentials directory {}: {error}",
                    path.display()
                )
            })?;
    }
    Ok(())
}

fn set_directory_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "Failed to set credentials directory permissions {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}
