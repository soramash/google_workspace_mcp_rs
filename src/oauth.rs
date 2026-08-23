//! OAuth and trusted-identity support for the native HTTP transport.
//!
//! All request authentication is derived from request headers. MCP session IDs are
//! deliberately not accepted as credentials or identity selectors.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fs2::FileExt;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::Mutex;
use url::Url;

use crate::auth::{LocalCredentialStore, StoredCredential, canonical_email};
use crate::native::common::safe_fetch::SafeFetcher;

const GOOGLE_AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_TOKENINFO_ENDPOINT: &str = "https://oauth2.googleapis.com/tokeninfo";
const GOOGLE_USERINFO_ENDPOINT: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const STATE_TTL_SECONDS: i64 = 600;
const ACCESS_TOKEN_TTL_SECONDS: i64 = 3_600;
const REFRESH_TOKEN_TTL_SECONDS: i64 = 30 * 24 * 3_600;
const MAX_HTTP_BODY: usize = 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const JWKS_CACHE_SECONDS: i64 = 300;
const DEFAULT_MAX_REGISTERED_CLIENTS: usize = 4_096;
const DEFAULT_REGISTERED_CLIENT_TTL_SECONDS: i64 = 86_400;
const MAX_CLIENT_REDIRECT_URIS: usize = 16;
const MAX_REDIRECT_URI_BYTES: usize = 2_048;
const MAX_CLIENT_NAME_BYTES: usize = 256;
const PROTOCOL_SCOPES: &str = "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";
const ALL_GOOGLE_SCOPES: &str = "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/calendar https://www.googleapis.com/auth/drive https://www.googleapis.com/auth/documents https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/gmail.settings.basic https://www.googleapis.com/auth/chat.messages https://www.googleapis.com/auth/chat.spaces https://www.googleapis.com/auth/spreadsheets https://www.googleapis.com/auth/forms.body https://www.googleapis.com/auth/forms.responses.readonly https://www.googleapis.com/auth/presentations https://www.googleapis.com/auth/tasks https://www.googleapis.com/auth/contacts https://www.googleapis.com/auth/cse https://www.googleapis.com/auth/script.projects https://www.googleapis.com/auth/script.deployments https://www.googleapis.com/auth/script.processes https://www.googleapis.com/auth/script.metrics https://www.googleapis.com/auth/script.external_request https://www.googleapis.com/auth/script.scriptapp";

/// The single authoritative authentication source selected at startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthMode {
    Legacy,
    InternalOAuth21,
    ExternalGoogle,
    TrustedGateway,
}

/// Canonical authenticated identity. `subject` is the verified provider subject,
/// not a caller-provided email or MCP session identifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Principal {
    pub email: String,
    pub subject: String,
}

/// Authentication material verified for one request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifiedRequest {
    pub principal: Principal,
    pub granted_scopes: Vec<String>,
    #[serde(skip_serializing)]
    pub google_access_token: Option<String>,
}

/// A transport-safe authentication error with an optional RFC 6750 challenge.
#[derive(Clone, Debug)]
pub struct AuthHttpError {
    pub status: StatusCode,
    pub www_authenticate: Option<String>,
    body: Value,
}

impl AuthHttpError {
    pub fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            www_authenticate: None,
            body: json!({ "error": code, "error_description": message }),
        }
    }

    pub fn bearer(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
        let scope = if code == "insufficient_scope" {
            ", scope=\"openid https://www.googleapis.com/auth/userinfo.email\""
        } else {
            ""
        };
        Self {
            status,
            www_authenticate: Some(format!(
                "Bearer error=\"{code}\", error_description=\"{escaped}\"{scope}"
            )),
            body: json!({ "error": code, "error_description": message }),
        }
    }

    pub fn json_body(&self) -> &Value {
        &self.body
    }

    fn invalid_request(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    fn invalid_client() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Client authentication failed",
        )
    }

    fn invalid_grant() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "The authorization grant is invalid or expired",
        )
    }

    fn unauthorized(message: &'static str) -> Self {
        Self::bearer(StatusCode::UNAUTHORIZED, "invalid_token", message)
    }

    fn gateway_unauthorized(message: &'static str) -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_gateway_assertion",
            message,
        )
    }

    fn server_error() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "The authorization server could not complete the request",
        )
    }
}

impl IntoResponse for AuthHttpError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body)).into_response();
        if let Some(challenge) = self.www_authenticate
            && let Ok(value) = HeaderValue::from_str(&challenge)
        {
            response.headers_mut().insert(WWW_AUTHENTICATE, value);
        }
        response
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub response_types_supported: Vec<&'static str>,
    pub grant_types_supported: Vec<&'static str>,
    pub token_endpoint_auth_methods_supported: Vec<&'static str>,
    pub code_challenge_methods_supported: Vec<&'static str>,
    pub scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProtectedResourceMetadata {
    pub resource: String,
    pub authorization_servers: Vec<String>,
    pub bearer_methods_supported: Vec<&'static str>,
    pub scopes_supported: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClientRegistrationRequest {
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClientRegistrationResponse {
    pub client_id: String,
    pub client_secret: String,
    pub client_id_issued_at: i64,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub token_endpoint_auth_method: &'static str,
}

#[derive(Clone, Debug)]
struct RegisteredClient {
    client_secret: String,
    redirect_uris: Vec<String>,
    expires_at: i64,
}

#[derive(Clone, Debug)]
struct GoogleCredential {
    access_token: String,
    refresh_token: Option<String>,
    scopes: Vec<String>,
    expires_at: i64,
    principal: Principal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum PendingPurpose {
    Internal {
        client_id: String,
        redirect_uri: String,
        client_state: Option<String>,
        client_code_challenge: String,
    },
    Legacy {
        expected_email: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingGoogleAuthorization {
    purpose: PendingPurpose,
    google_code_verifier: String,
    requested_scopes: Vec<String>,
    expires_at: i64,
}

#[derive(Clone, Debug)]
struct AuthorizationCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    client_scopes: Vec<String>,
    credential: GoogleCredential,
    expires_at: i64,
}

#[derive(Clone, Debug)]
struct AccessReference {
    client_id: String,
    credential: GoogleCredential,
    granted_scopes: Vec<String>,
    expires_at: i64,
}

#[derive(Clone, Debug)]
struct RefreshReference {
    client_id: String,
    credential: GoogleCredential,
    granted_scopes: Vec<String>,
    expires_at: i64,
}

#[derive(Debug, Default)]
struct RuntimeStore {
    clients: HashMap<String, RegisteredClient>,
    pending: HashMap<String, PendingGoogleAuthorization>,
    authorization_codes: HashMap<String, AuthorizationCode>,
    access_tokens: HashMap<String, AccessReference>,
    refresh_tokens: HashMap<String, RefreshReference>,
}

impl RuntimeStore {
    fn clean_expired(&mut self, now: i64) {
        self.clients.retain(|_, value| value.expires_at > now);
        self.pending.retain(|_, value| value.expires_at > now);
        self.authorization_codes
            .retain(|_, value| value.expires_at > now);
        self.access_tokens.retain(|_, value| value.expires_at > now);
        self.refresh_tokens
            .retain(|_, value| value.expires_at > now);
    }
}

#[derive(Debug)]
struct CachedJwks {
    set: JwkSet,
    expires_at: i64,
}

#[derive(Debug)]
struct GatewayConfig {
    header: HeaderName,
    jwks_url: Url,
    algorithms: Vec<Algorithm>,
    audience: String,
    issuer: Option<String>,
    cache: Mutex<Option<CachedJwks>>,
}

/// Process runtime shared behind `Arc` by the HTTP transport.
pub struct OAuthRuntime {
    mode: AuthMode,
    stateless: bool,
    public_url: Url,
    resource_url: Url,
    google_client_id: Option<String>,
    google_client_secret: Option<String>,
    google_redirect_uri: Url,
    scopes: Vec<String>,
    allowed_redirects: Vec<AllowedRedirect>,
    local_signing_key: Option<Vec<u8>>,
    fixed_principal: Option<Principal>,
    gateway: Option<GatewayConfig>,
    http: reqwest::Client,
    max_registered_clients: usize,
    registered_client_ttl_seconds: i64,
    store: Mutex<RuntimeStore>,
}

#[derive(Clone, Debug)]
enum AllowedRedirect {
    Exact(String),
    LoopbackAnyPort {
        scheme: String,
        host: String,
        path: String,
        query: Option<String>,
    },
}

impl OAuthRuntime {
    /// Validate startup environment and construct an Arc-ready runtime.
    pub fn from_env() -> Result<Arc<Self>, String> {
        let oauth21 = env_bool("MCP_ENABLE_OAUTH21")?.unwrap_or(false);
        let external = env_bool("EXTERNAL_OAUTH21_PROVIDER")?.unwrap_or(false);
        let gateway_enabled = env_bool("TRUST_GATEWAY_IDENTITY")?.unwrap_or(false);
        let stateless = env_bool("WORKSPACE_MCP_STATELESS_MODE")?.unwrap_or(false);
        let service_file = non_empty_env("GOOGLE_SERVICE_ACCOUNT_KEY_FILE")?;
        let service_json = non_empty_env("GOOGLE_SERVICE_ACCOUNT_KEY_JSON")?;

        if service_file.is_some() && service_json.is_some() {
            return Err(String::from(
                "GOOGLE_SERVICE_ACCOUNT_KEY_FILE and GOOGLE_SERVICE_ACCOUNT_KEY_JSON are mutually exclusive",
            ));
        }
        if external && !oauth21 {
            return Err(String::from(
                "EXTERNAL_OAUTH21_PROVIDER requires MCP_ENABLE_OAUTH21=true",
            ));
        }
        if gateway_enabled && oauth21 {
            return Err(String::from(
                "TRUST_GATEWAY_IDENTITY is incompatible with MCP_ENABLE_OAUTH21=true",
            ));
        }
        if stateless && !oauth21 {
            return Err(String::from(
                "WORKSPACE_MCP_STATELESS_MODE requires MCP_ENABLE_OAUTH21=true",
            ));
        }
        if oauth21 && (service_file.is_some() || service_json.is_some()) {
            return Err(String::from(
                "Service account mode is incompatible with MCP OAuth 2.1 mode",
            ));
        }

        let mode = if gateway_enabled {
            AuthMode::TrustedGateway
        } else if oauth21 && external {
            AuthMode::ExternalGoogle
        } else if oauth21 {
            AuthMode::InternalOAuth21
        } else {
            AuthMode::Legacy
        };

        let public_url = effective_public_url()?;
        let resource_url = public_url
            .join("mcp")
            .map_err(|_| String::from("Failed to construct the OAuth resource URL"))?;
        let google_redirect_uri = match non_empty_env("GOOGLE_OAUTH_REDIRECT_URI")? {
            Some(value) => Url::parse(&value)
                .map_err(|_| String::from("GOOGLE_OAUTH_REDIRECT_URI must be an absolute URL"))?,
            None => public_url
                .join("oauth2callback")
                .map_err(|_| String::from("Failed to construct the Google callback URL"))?,
        };
        require_secure_http_url(&google_redirect_uri, "GOOGLE_OAUTH_REDIRECT_URI")?;

        let google_client_id = non_empty_env("GOOGLE_OAUTH_CLIENT_ID")?;
        let google_client_secret = non_empty_env("GOOGLE_OAUTH_CLIENT_SECRET")?;
        match mode {
            AuthMode::InternalOAuth21 => {
                if google_client_id.is_none() || google_client_secret.is_none() {
                    return Err(String::from(
                        "Internal OAuth 2.1 requires GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET",
                    ));
                }
            }
            AuthMode::ExternalGoogle if google_client_id.is_none() => {
                return Err(String::from(
                    "External Google OAuth requires GOOGLE_OAUTH_CLIENT_ID",
                ));
            }
            _ => {}
        }

        let scopes = configured_scopes();
        let allowed_redirects = if mode == AuthMode::InternalOAuth21 {
            parse_allowed_redirects(
                non_empty_env("WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS")?
                    .as_deref()
                    .ok_or_else(|| {
                        String::from(
                            "Internal OAuth 2.1 requires WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS",
                        )
                    })?,
            )?
        } else {
            Vec::new()
        };

        let local_signing_key = if mode == AuthMode::InternalOAuth21 {
            let material = non_empty_env("FASTMCP_SERVER_AUTH_GOOGLE_JWT_SIGNING_KEY")?
                .or_else(|| google_client_secret.clone())
                .ok_or_else(|| String::from("Internal OAuth signing key is not configured"))?;
            if material.len() < 32 {
                return Err(String::from(
                    "Internal OAuth signing material must contain at least 32 bytes",
                ));
            }
            Some(Sha256::digest(material.as_bytes()).to_vec())
        } else {
            None
        };

        let fixed_principal = if matches!(mode, AuthMode::Legacy) {
            let email = non_empty_env("USER_GOOGLE_EMAIL")?.ok_or_else(|| {
                String::from("Legacy mode requires USER_GOOGLE_EMAIL as the fixed principal")
            })?;
            let email = canonical_email(&email)?;
            Some(Principal {
                subject: email.clone(),
                email,
            })
        } else {
            None
        };

        let gateway = if mode == AuthMode::TrustedGateway {
            Some(GatewayConfig::from_env()?)
        } else {
            None
        };
        if mode == AuthMode::TrustedGateway
            && (service_file.is_some() || service_json.is_some())
            && configured_domains().is_empty()
        {
            return Err(String::from(
                "Trusted-gateway domain-wide delegation requires DWD_ALLOWED_DOMAINS",
            ));
        }

        let max_registered_clients = positive_usize_env(
            "WORKSPACE_MCP_MAX_REGISTERED_CLIENTS",
            DEFAULT_MAX_REGISTERED_CLIENTS,
        )?;
        let registered_client_ttl_seconds = positive_i64_env(
            "WORKSPACE_MCP_REGISTERED_CLIENT_TTL_SECONDS",
            DEFAULT_REGISTERED_CLIENT_TTL_SECONDS,
        )?;
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| String::from("Failed to initialize OAuth HTTP client"))?;

        Ok(Arc::new(Self {
            mode,
            stateless,
            public_url,
            resource_url,
            google_client_id,
            google_client_secret,
            google_redirect_uri,
            scopes,
            allowed_redirects,
            local_signing_key,
            fixed_principal,
            gateway,
            http,
            max_registered_clients,
            registered_client_ttl_seconds,
            store: Mutex::new(RuntimeStore::default()),
        }))
    }

    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    pub fn stateless(&self) -> bool {
        self.stateless
    }

    pub fn public_url(&self) -> &Url {
        &self.public_url
    }

    pub fn google_callback_path(&self) -> &str {
        self.google_redirect_uri.path()
    }

    pub fn error_response(&self, mut error: AuthHttpError) -> Response {
        if matches!(
            self.mode,
            AuthMode::InternalOAuth21 | AuthMode::ExternalGoogle
        ) && let Some(challenge) = error.www_authenticate.as_mut()
            && let Ok(metadata) = self
                .public_url
                .join(".well-known/oauth-protected-resource/mcp")
        {
            challenge.push_str(&format!(", resource_metadata=\"{}\"", metadata.as_str()));
        }
        error.into_response()
    }

    pub fn authorization_server_metadata(&self) -> AuthorizationServerMetadata {
        AuthorizationServerMetadata {
            issuer: self.public_url.to_string(),
            authorization_endpoint: self.endpoint("authorize"),
            token_endpoint: self.endpoint("token"),
            registration_endpoint: self.endpoint("register"),
            response_types_supported: vec!["code"],
            grant_types_supported: vec!["authorization_code", "refresh_token"],
            token_endpoint_auth_methods_supported: vec!["client_secret_post"],
            code_challenge_methods_supported: vec!["S256"],
            scopes_supported: self.scopes.clone(),
        }
    }

    pub fn protected_resource_metadata(&self) -> ProtectedResourceMetadata {
        let authorization_server = if self.mode == AuthMode::ExternalGoogle {
            String::from("https://accounts.google.com")
        } else {
            self.public_url.to_string()
        };
        ProtectedResourceMetadata {
            resource: self.resource_url.to_string(),
            authorization_servers: vec![authorization_server],
            bearer_methods_supported: vec!["header"],
            scopes_supported: self.scopes.clone(),
        }
    }

    /// Authenticate only from the current request headers. `Mcp-Session-Id` is
    /// intentionally never inspected.
    pub async fn authenticate_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<VerifiedRequest, AuthHttpError> {
        match self.mode {
            AuthMode::Legacy => {
                let principal = self.fixed_principal.clone().ok_or_else(|| {
                    AuthHttpError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "server_error",
                        "Legacy principal is not configured",
                    )
                })?;
                Ok(VerifiedRequest {
                    principal,
                    granted_scopes: Vec::new(),
                    google_access_token: None,
                })
            }
            AuthMode::TrustedGateway => self.authenticate_gateway(headers).await,
            AuthMode::ExternalGoogle => {
                let bearer = require_bearer(headers)?;
                self.verify_external_google(&bearer).await
            }
            AuthMode::InternalOAuth21 => {
                let bearer = require_bearer(headers)?;
                self.verify_local_bearer(&bearer).await
            }
        }
    }

    async fn authenticate_gateway(
        &self,
        headers: &HeaderMap,
    ) -> Result<VerifiedRequest, AuthHttpError> {
        let config = self
            .gateway
            .as_ref()
            .ok_or_else(AuthHttpError::server_error)?;
        let assertion = headers
            .get(&config.header)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AuthHttpError::gateway_unauthorized("A trusted gateway assertion is required")
            })?;
        let header = decode_header(assertion)
            .map_err(|_| AuthHttpError::gateway_unauthorized("The gateway assertion is invalid"))?;
        let kid = header
            .kid
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AuthHttpError::gateway_unauthorized("The gateway assertion has no key id")
            })?;
        if !config.algorithms.contains(&header.alg) {
            return Err(AuthHttpError::gateway_unauthorized(
                "The gateway assertion algorithm is not allowed",
            ));
        }

        let set = self.gateway_jwks(false).await?;
        let jwk = if let Some(jwk) = set.find(kid) {
            jwk.clone()
        } else {
            self.gateway_jwks(true)
                .await?
                .find(kid)
                .cloned()
                .ok_or_else(|| {
                    AuthHttpError::gateway_unauthorized("The gateway signing key is unknown")
                })?
        };
        let key = DecodingKey::from_jwk(&jwk).map_err(|_| {
            AuthHttpError::gateway_unauthorized("The gateway signing key is invalid")
        })?;
        let mut validation = Validation::new(header.alg);
        validation.leeway = 0;
        validation.set_audience(&[config.audience.as_str()]);
        validation.required_spec_claims.insert(String::from("exp"));
        if let Some(issuer) = config.issuer.as_deref() {
            validation.set_issuer(&[issuer]);
        }
        let verified = decode::<GatewayClaims>(assertion, &key, &validation).map_err(|_| {
            AuthHttpError::gateway_unauthorized("The gateway assertion was rejected")
        })?;
        if verified.claims.exp <= now() {
            return Err(AuthHttpError::gateway_unauthorized(
                "The gateway assertion has expired",
            ));
        }
        if !verified.claims.aud.exactly(&config.audience) {
            return Err(AuthHttpError::gateway_unauthorized(
                "The gateway assertion audience is not accepted",
            ));
        }
        let email = canonical_email(&verified.claims.email).map_err(|_| {
            AuthHttpError::gateway_unauthorized("The gateway email claim is invalid")
        })?;
        let subject = verified
            .claims
            .sub
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| email.clone());
        Ok(VerifiedRequest {
            principal: Principal { email, subject },
            granted_scopes: split_scopes(verified.claims.scope.as_deref().unwrap_or("")),
            google_access_token: None,
        })
    }

    async fn gateway_jwks(&self, force: bool) -> Result<JwkSet, AuthHttpError> {
        let config = self
            .gateway
            .as_ref()
            .ok_or_else(AuthHttpError::server_error)?;
        let now = now();
        {
            let cache = config.cache.lock().await;
            if !force
                && let Some(cached) = cache.as_ref()
                && cached.expires_at > now
            {
                return Ok(cached.set.clone());
            }
        }
        let body = fetch_jwks(&config.jwks_url).await.map_err(|_| {
            AuthHttpError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Gateway verification keys are unavailable",
            )
        })?;
        let set: JwkSet = serde_json::from_slice(&body).map_err(|_| {
            AuthHttpError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Gateway verification keys are invalid",
            )
        })?;
        *config.cache.lock().await = Some(CachedJwks {
            set: set.clone(),
            expires_at: now + JWKS_CACHE_SECONDS,
        });
        Ok(set)
    }

    async fn verify_external_google(
        &self,
        access_token: &str,
    ) -> Result<VerifiedRequest, AuthHttpError> {
        let client_id = self
            .google_client_id
            .as_deref()
            .ok_or_else(AuthHttpError::server_error)?;
        let response = self
            .http
            .get(GOOGLE_TOKENINFO_ENDPOINT)
            .query(&[("access_token", access_token)])
            .send()
            .await
            .map_err(|_| AuthHttpError::unauthorized("Google could not verify the bearer token"))?;
        if !response.status().is_success() {
            return Err(AuthHttpError::unauthorized(
                "Google rejected the bearer token",
            ));
        }
        let tokeninfo: GoogleTokenInfo = bounded_json(response).await?;
        let audience = match (tokeninfo.audience.as_deref(), tokeninfo.aud.as_deref()) {
            (Some(left), Some(right)) if left != right => {
                return Err(AuthHttpError::unauthorized(
                    "The Google token audience is ambiguous",
                ));
            }
            (Some(value), _) | (_, Some(value)) => value,
            (None, None) => {
                return Err(AuthHttpError::unauthorized(
                    "The Google token has no audience",
                ));
            }
        };
        if !constant_time_eq(audience.as_bytes(), client_id.as_bytes()) {
            return Err(AuthHttpError::unauthorized(
                "The Google token audience is not accepted",
            ));
        }
        if tokeninfo.expires_in.as_i64().unwrap_or(0) <= 0 {
            return Err(AuthHttpError::unauthorized("The Google token has expired"));
        }
        let subject = tokeninfo
            .sub
            .or(tokeninfo.user_id)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AuthHttpError::unauthorized("The Google token has no subject"))?;
        let scopes =
            split_scopes(tokeninfo.scope.as_deref().ok_or_else(|| {
                AuthHttpError::unauthorized("Google did not report token scopes")
            })?);
        if !has_protocol_scopes(&scopes) {
            return Err(AuthHttpError::bearer(
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                "The Google token does not include the required protocol scopes",
            ));
        }
        let userinfo = self.google_userinfo(access_token).await?;
        Ok(VerifiedRequest {
            principal: Principal {
                email: userinfo.email,
                subject,
            },
            granted_scopes: scopes,
            google_access_token: Some(access_token.to_owned()),
        })
    }

    async fn verify_local_bearer(&self, token: &str) -> Result<VerifiedRequest, AuthHttpError> {
        let key = self
            .local_signing_key
            .as_deref()
            .ok_or_else(AuthHttpError::server_error)?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        validation.set_issuer(&[self.public_url.as_str()]);
        validation.set_audience(&[self.resource_url.as_str()]);
        validation
            .required_spec_claims
            .extend(["exp", "iss", "aud", "sub"].into_iter().map(String::from));
        let claims =
            decode::<LocalAccessClaims>(token, &DecodingKey::from_secret(key), &validation)
                .map_err(|_| AuthHttpError::unauthorized("The bearer token is invalid or expired"))?
                .claims;
        if claims.token_use != "access" || claims.jti.is_empty() {
            return Err(AuthHttpError::unauthorized(
                "The bearer token is not an access token",
            ));
        }
        let mut store = self.store.lock().await;
        store.clean_expired(now());
        let reference = store
            .access_tokens
            .get(&claims.jti)
            .filter(|entry| entry.client_id == claims.client_id)
            .ok_or_else(|| AuthHttpError::unauthorized("The bearer token is no longer active"))?;
        if reference.credential.principal.subject != claims.sub
            || reference.granted_scopes != claims.scopes
        {
            return Err(AuthHttpError::unauthorized(
                "The bearer token reference does not match",
            ));
        }
        if !has_protocol_scopes(&reference.granted_scopes) {
            return Err(AuthHttpError::bearer(
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                "The bearer token does not include the required protocol scopes",
            ));
        }
        Ok(VerifiedRequest {
            principal: reference.credential.principal.clone(),
            granted_scopes: reference.granted_scopes.clone(),
            google_access_token: Some(reference.credential.access_token.clone()),
        })
    }

    async fn google_userinfo(&self, token: &str) -> Result<GoogleUserInfo, AuthHttpError> {
        let response = self
            .http
            .get(GOOGLE_USERINFO_ENDPOINT)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| AuthHttpError::unauthorized("Google user information is unavailable"))?;
        if !response.status().is_success() {
            return Err(AuthHttpError::unauthorized(
                "Google rejected the access token",
            ));
        }
        let value: GoogleUserInfoResponse = bounded_json(response).await?;
        if value.email_verified == Some(false) {
            return Err(AuthHttpError::unauthorized(
                "The Google account email is not verified",
            ));
        }
        let email = canonical_email(&value.email)
            .map_err(|_| AuthHttpError::unauthorized("Google returned an invalid email"))?;
        let subject = value.sub.filter(|value| !value.is_empty());
        Ok(GoogleUserInfo { email, subject })
    }

    fn endpoint(&self, path: &str) -> String {
        self.public_url
            .join(path)
            .map_or_else(|_| self.public_url.to_string(), |url| url.to_string())
    }

    fn redirect_allowed(&self, candidate: &str) -> bool {
        self.allowed_redirects
            .iter()
            .any(|allowed| allowed.matches(candidate))
    }

    async fn register_client(
        &self,
        request: ClientRegistrationRequest,
    ) -> Result<ClientRegistrationResponse, AuthHttpError> {
        if self.mode != AuthMode::InternalOAuth21 {
            return Err(AuthHttpError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "Dynamic client registration is unavailable",
            ));
        }
        if request.redirect_uris.is_empty()
            || request.redirect_uris.len() > MAX_CLIENT_REDIRECT_URIS
            || request
                .redirect_uris
                .iter()
                .any(|uri| uri.len() > MAX_REDIRECT_URI_BYTES || !self.redirect_allowed(uri))
        {
            return Err(AuthHttpError::invalid_request(
                "Every redirect URI must match the configured allowlist and size limits",
            ));
        }
        if request
            .client_name
            .as_ref()
            .is_some_and(|name| name.len() > MAX_CLIENT_NAME_BYTES)
        {
            return Err(AuthHttpError::invalid_request(
                "client_name exceeds the maximum size",
            ));
        }
        if request
            .token_endpoint_auth_method
            .as_deref()
            .is_some_and(|method| method != "client_secret_post")
        {
            return Err(AuthHttpError::invalid_request(
                "Only client_secret_post is supported",
            ));
        }

        let issued_at = now();
        let expires_at = issued_at
            .checked_add(self.registered_client_ttl_seconds)
            .ok_or_else(AuthHttpError::server_error)?;
        let client_secret = random_opaque(32).map_err(|_| AuthHttpError::server_error())?;
        let mut store = self.store.lock().await;
        store.clean_expired(issued_at);
        if store.clients.len() >= self.max_registered_clients {
            return Err(AuthHttpError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_requests",
                "The dynamic client registration limit has been reached",
            ));
        }
        let client_id = loop {
            let candidate = random_opaque(24).map_err(|_| AuthHttpError::server_error())?;
            if !store.clients.contains_key(&candidate) {
                break candidate;
            }
        };
        store.clients.insert(
            client_id.clone(),
            RegisteredClient {
                client_secret: client_secret.clone(),
                redirect_uris: request.redirect_uris.clone(),
                expires_at,
            },
        );
        Ok(ClientRegistrationResponse {
            client_id,
            client_secret,
            client_id_issued_at: issued_at,
            redirect_uris: request.redirect_uris,
            client_name: request.client_name,
            token_endpoint_auth_method: "client_secret_post",
        })
    }

    async fn begin_internal_authorization(
        &self,
        query: AuthorizeQuery,
    ) -> Result<Url, AuthHttpError> {
        if self.mode != AuthMode::InternalOAuth21 {
            return Err(AuthHttpError::invalid_request(
                "The authorization endpoint is unavailable",
            ));
        }
        if query.response_type != "code"
            || query.code_challenge_method.as_deref() != Some("S256")
            || !valid_pkce_challenge(&query.code_challenge)
        {
            return Err(AuthHttpError::invalid_request(
                "response_type=code and PKCE S256 are required",
            ));
        }
        let requested_scopes = split_scopes(query.scope.as_deref().unwrap_or(PROTOCOL_SCOPES));
        if requested_scopes.is_empty()
            || !has_protocol_scopes(&requested_scopes)
            || requested_scopes
                .iter()
                .any(|scope| !self.scopes.contains(scope))
        {
            return Err(AuthHttpError::new(
                StatusCode::BAD_REQUEST,
                "invalid_scope",
                "One or more requested scopes are not supported",
            ));
        }
        {
            let mut store = self.store.lock().await;
            store.clean_expired(now());
            let client = store
                .clients
                .get(&query.client_id)
                .ok_or_else(AuthHttpError::invalid_client)?;
            if !client
                .redirect_uris
                .iter()
                .any(|uri| uri == &query.redirect_uri)
                || !self.redirect_allowed(&query.redirect_uri)
            {
                return Err(AuthHttpError::invalid_request(
                    "The redirect URI is not registered",
                ));
            }
        }
        let google_state = random_opaque(24).map_err(|_| AuthHttpError::server_error())?;
        let google_verifier = random_opaque(48).map_err(|_| AuthHttpError::server_error())?;
        let google_challenge = pkce_challenge(&google_verifier);
        let pending = PendingGoogleAuthorization {
            purpose: PendingPurpose::Internal {
                client_id: query.client_id,
                redirect_uri: query.redirect_uri,
                client_state: query.state,
                client_code_challenge: query.code_challenge,
            },
            google_code_verifier: google_verifier,
            requested_scopes: requested_scopes.clone(),
            expires_at: now() + STATE_TTL_SECONDS,
        };
        self.store
            .lock()
            .await
            .pending
            .insert(google_state.clone(), pending);
        self.google_authorization_url(
            &google_state,
            &google_challenge,
            &requested_scopes,
            None,
            "consent",
        )
        .map_err(|_| AuthHttpError::server_error())
    }

    fn google_authorization_url(
        &self,
        state: &str,
        challenge: &str,
        scopes: &[String],
        login_hint: Option<&str>,
        prompt: &str,
    ) -> Result<Url, String> {
        let client_id = self
            .google_client_id
            .as_deref()
            .ok_or_else(|| String::from("GOOGLE_OAUTH_CLIENT_ID is required"))?;
        let mut url = Url::parse(GOOGLE_AUTHORIZATION_ENDPOINT)
            .map_err(|_| String::from("Google authorization endpoint is invalid"))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("client_id", client_id)
                .append_pair("redirect_uri", self.google_redirect_uri.as_str())
                .append_pair("response_type", "code")
                .append_pair("scope", &scopes.join(" "))
                .append_pair("state", state)
                .append_pair("code_challenge", challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("access_type", "offline")
                .append_pair("prompt", prompt)
                .append_pair("include_granted_scopes", "true");
            if let Some(email) = login_hint {
                pairs.append_pair("login_hint", email);
            }
        }
        Ok(url)
    }

    async fn exchange_google_code(
        &self,
        code: &str,
        verifier: &str,
    ) -> Result<GoogleCredential, AuthHttpError> {
        let client_id = self
            .google_client_id
            .as_deref()
            .ok_or_else(AuthHttpError::server_error)?;
        let client_secret = self
            .google_client_secret
            .as_deref()
            .ok_or_else(AuthHttpError::server_error)?;
        let response = self
            .http
            .post(GOOGLE_TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("redirect_uri", self.google_redirect_uri.as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|_| AuthHttpError::invalid_grant())?;
        if !response.status().is_success() {
            return Err(AuthHttpError::invalid_grant());
        }
        let token: GoogleTokenResponse = bounded_json(response).await?;
        if token.access_token.is_empty() || token.expires_in.as_i64().unwrap_or(0) <= 0 {
            return Err(AuthHttpError::invalid_grant());
        }
        let scopes = token
            .scope
            .as_deref()
            .map(split_scopes)
            .filter(|scopes| !scopes.is_empty())
            .ok_or_else(|| {
                AuthHttpError::new(
                    StatusCode::BAD_GATEWAY,
                    "invalid_grant",
                    "Google did not report the granted scopes",
                )
            })?;
        let userinfo = self.google_userinfo(&token.access_token).await?;
        let subject = userinfo.subject.unwrap_or_else(|| userinfo.email.clone());
        Ok(GoogleCredential {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            scopes,
            expires_at: now() + token.expires_in.as_i64().unwrap_or(0),
            principal: Principal {
                email: userinfo.email,
                subject,
            },
        })
    }

    async fn complete_google_callback(
        &self,
        query: CallbackQuery,
    ) -> Result<Response, AuthHttpError> {
        let state = query
            .state
            .as_deref()
            .ok_or_else(AuthHttpError::invalid_grant)?;
        let pending = {
            let mut store = self.store.lock().await;
            store.clean_expired(now());
            store.pending.remove(state)
        }
        .or_else(|| take_legacy_pending(state))
        .ok_or_else(AuthHttpError::invalid_grant)?;
        if pending.expires_at <= now() {
            return Err(AuthHttpError::invalid_grant());
        }
        if query.error.is_some() {
            return match &pending.purpose {
                PendingPurpose::Internal {
                    redirect_uri,
                    client_state,
                    ..
                } => {
                    let mut target = Url::parse(redirect_uri)
                        .map_err(|_| AuthHttpError::invalid_request("Redirect URI is invalid"))?;
                    target
                        .query_pairs_mut()
                        .append_pair("error", "access_denied");
                    if let Some(client_state) = client_state {
                        target.query_pairs_mut().append_pair("state", client_state);
                    }
                    Ok(Redirect::to(target.as_str()).into_response())
                }
                PendingPurpose::Legacy { .. } => {
                    Ok(safe_html("Google authorization was denied.").into_response())
                }
            };
        }
        let code = query
            .code
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(AuthHttpError::invalid_grant)?;
        let credential = self
            .exchange_google_code(code, &pending.google_code_verifier)
            .await?;
        match pending.purpose {
            PendingPurpose::Internal {
                client_id,
                redirect_uri,
                client_state,
                client_code_challenge,
            } => {
                let client_scopes = pending
                    .requested_scopes
                    .iter()
                    .filter(|scope| credential.scopes.contains(scope))
                    .cloned()
                    .collect::<Vec<_>>();
                if !has_protocol_scopes(&client_scopes) {
                    return Err(AuthHttpError::new(
                        StatusCode::FORBIDDEN,
                        "insufficient_scope",
                        "Google did not grant the required protocol scopes",
                    ));
                }
                let local_code = random_opaque(32).map_err(|_| AuthHttpError::server_error())?;
                self.store.lock().await.authorization_codes.insert(
                    local_code.clone(),
                    AuthorizationCode {
                        client_id,
                        redirect_uri: redirect_uri.clone(),
                        code_challenge: client_code_challenge,
                        client_scopes,
                        credential,
                        expires_at: now() + STATE_TTL_SECONDS,
                    },
                );
                let mut target = Url::parse(&redirect_uri)
                    .map_err(|_| AuthHttpError::invalid_request("Redirect URI is invalid"))?;
                target.query_pairs_mut().append_pair("code", &local_code);
                if let Some(client_state) = client_state {
                    target.query_pairs_mut().append_pair("state", &client_state);
                }
                Ok(Redirect::to(target.as_str()).into_response())
            }
            PendingPurpose::Legacy { expected_email } => {
                if !credential
                    .principal
                    .email
                    .eq_ignore_ascii_case(&expected_email)
                {
                    return Ok(safe_html(
                        "The authorized Google account does not match the expected account.",
                    )
                    .into_response());
                }
                if self.stateless {
                    return Err(AuthHttpError::server_error());
                }
                let store =
                    LocalCredentialStore::from_env().map_err(|_| AuthHttpError::server_error())?;
                let prior_refresh = store
                    .load(&expected_email)
                    .map_err(|_| AuthHttpError::server_error())?
                    .and_then(|value| value.refresh_token);
                let refresh_token = credential.refresh_token.or(prior_refresh);
                let expiry = OffsetDateTime::from_unix_timestamp(credential.expires_at)
                    .ok()
                    .and_then(|value| value.format(&Rfc3339).ok());
                store
                    .store(
                        &expected_email,
                        &StoredCredential {
                            token: Some(credential.access_token),
                            refresh_token,
                            token_uri: Some(String::from(GOOGLE_TOKEN_ENDPOINT)),
                            client_id: self.google_client_id.clone(),
                            client_secret: self.google_client_secret.clone(),
                            scopes: credential.scopes,
                            expiry,
                        },
                    )
                    .map_err(|_| AuthHttpError::server_error())?;
                Ok(
                    safe_html("Google authorization completed. You may close this window.")
                        .into_response(),
                )
            }
        }
    }

    async fn exchange_token(&self, form: TokenRequest) -> Result<TokenResponse, AuthHttpError> {
        if self.mode != AuthMode::InternalOAuth21 {
            return Err(AuthHttpError::invalid_request(
                "The token endpoint is unavailable",
            ));
        }
        self.validate_client(&form.client_id, &form.client_secret)
            .await?;
        match form.grant_type.as_str() {
            "authorization_code" => self.exchange_authorization_code(form).await,
            "refresh_token" => self.exchange_refresh_token(form).await,
            _ => Err(AuthHttpError::new(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "The requested grant type is not supported",
            )),
        }
    }

    async fn validate_client(&self, client_id: &str, secret: &str) -> Result<(), AuthHttpError> {
        let mut store = self.store.lock().await;
        store.clean_expired(now());
        let valid = store.clients.get(client_id).is_some_and(|client| {
            constant_time_eq(client.client_secret.as_bytes(), secret.as_bytes())
        });
        if valid {
            Ok(())
        } else {
            Err(AuthHttpError::invalid_client())
        }
    }

    async fn exchange_authorization_code(
        &self,
        form: TokenRequest,
    ) -> Result<TokenResponse, AuthHttpError> {
        let code = form
            .code
            .as_deref()
            .ok_or_else(AuthHttpError::invalid_grant)?;
        let verifier = form
            .code_verifier
            .as_deref()
            .ok_or_else(AuthHttpError::invalid_grant)?;
        let authorization = {
            let mut store = self.store.lock().await;
            store.clean_expired(now());
            store.authorization_codes.remove(code)
        }
        .ok_or_else(AuthHttpError::invalid_grant)?;
        if !valid_pkce_verifier(verifier)
            || authorization.client_id != form.client_id
            || form.redirect_uri.as_deref() != Some(authorization.redirect_uri.as_str())
            || !constant_time_eq(
                pkce_challenge(verifier).as_bytes(),
                authorization.code_challenge.as_bytes(),
            )
        {
            return Err(AuthHttpError::invalid_grant());
        }
        self.issue_tokens(
            &form.client_id,
            authorization.credential,
            authorization.client_scopes,
            None,
        )
        .await
    }

    async fn exchange_refresh_token(
        &self,
        form: TokenRequest,
    ) -> Result<TokenResponse, AuthHttpError> {
        let opaque = form
            .refresh_token
            .as_deref()
            .ok_or_else(AuthHttpError::invalid_grant)?;
        let reference = {
            let mut store = self.store.lock().await;
            store.clean_expired(now());
            store.refresh_tokens.get(opaque).cloned()
        }
        .ok_or_else(AuthHttpError::invalid_grant)?;
        if reference.client_id != form.client_id {
            return Err(AuthHttpError::invalid_grant());
        }
        let credential = self.refresh_google(reference.credential).await?;
        let granted_scopes = reference
            .granted_scopes
            .into_iter()
            .filter(|scope| credential.scopes.contains(scope))
            .collect::<Vec<_>>();
        if !has_protocol_scopes(&granted_scopes) {
            return Err(AuthHttpError::invalid_grant());
        }
        self.issue_tokens(&form.client_id, credential, granted_scopes, Some(opaque))
            .await
    }

    async fn refresh_google(
        &self,
        mut credential: GoogleCredential,
    ) -> Result<GoogleCredential, AuthHttpError> {
        let Some(refresh_token) = credential.refresh_token.as_deref() else {
            if credential.expires_at > now() {
                return Ok(credential);
            }
            return Err(AuthHttpError::invalid_grant());
        };
        let response = self
            .http
            .post(GOOGLE_TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "refresh_token"),
                (
                    "client_id",
                    self.google_client_id
                        .as_deref()
                        .ok_or_else(AuthHttpError::server_error)?,
                ),
                (
                    "client_secret",
                    self.google_client_secret
                        .as_deref()
                        .ok_or_else(AuthHttpError::server_error)?,
                ),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|_| AuthHttpError::invalid_grant())?;
        if !response.status().is_success() {
            return Err(AuthHttpError::invalid_grant());
        }
        let refreshed: GoogleTokenResponse = bounded_json(response).await?;
        if refreshed.access_token.is_empty() || refreshed.expires_in.as_i64().unwrap_or(0) <= 0 {
            return Err(AuthHttpError::invalid_grant());
        }
        credential.access_token = refreshed.access_token;
        credential.expires_at = now() + refreshed.expires_in.as_i64().unwrap_or(0);
        if let Some(rotated) = refreshed.refresh_token.filter(|value| !value.is_empty()) {
            credential.refresh_token = Some(rotated);
        }
        if let Some(scope) = refreshed.scope {
            let actual = split_scopes(&scope);
            if actual.is_empty() {
                return Err(AuthHttpError::invalid_grant());
            }
            credential.scopes = actual;
        }
        Ok(credential)
    }

    async fn issue_tokens(
        &self,
        client_id: &str,
        credential: GoogleCredential,
        granted_scopes: Vec<String>,
        old_refresh: Option<&str>,
    ) -> Result<TokenResponse, AuthHttpError> {
        let jti = random_opaque(24).map_err(|_| AuthHttpError::server_error())?;
        let refresh_token = random_opaque(32).map_err(|_| AuthHttpError::server_error())?;
        let issued = now();
        let expiry = issued + ACCESS_TOKEN_TTL_SECONDS.min(credential.expires_at - issued);
        if expiry <= issued {
            return Err(AuthHttpError::invalid_grant());
        }
        let claims = LocalAccessClaims {
            iss: self.public_url.to_string(),
            aud: self.resource_url.to_string(),
            exp: expiry,
            iat: issued,
            jti: jti.clone(),
            sub: credential.principal.subject.clone(),
            token_use: String::from("access"),
            scopes: granted_scopes.clone(),
            client_id: client_id.to_owned(),
        };
        let signing_key = self
            .local_signing_key
            .as_deref()
            .ok_or_else(AuthHttpError::server_error)?;
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(signing_key),
        )
        .map_err(|_| AuthHttpError::server_error())?;
        let mut store = self.store.lock().await;
        store.clean_expired(issued);
        if let Some(old) = old_refresh
            && store.refresh_tokens.remove(old).is_none()
        {
            return Err(AuthHttpError::invalid_grant());
        }
        store.access_tokens.insert(
            jti,
            AccessReference {
                client_id: client_id.to_owned(),
                credential: credential.clone(),
                granted_scopes: granted_scopes.clone(),
                expires_at: expiry,
            },
        );
        store.refresh_tokens.insert(
            refresh_token.clone(),
            RefreshReference {
                client_id: client_id.to_owned(),
                credential: credential.clone(),
                granted_scopes: granted_scopes.clone(),
                expires_at: issued + REFRESH_TOKEN_TTL_SECONDS,
            },
        );
        Ok(TokenResponse {
            access_token: token,
            token_type: "Bearer",
            expires_in: expiry - issued,
            refresh_token,
            scope: granted_scopes.join(" "),
        })
    }
}

impl GatewayConfig {
    fn from_env() -> Result<Self, String> {
        let header_value = non_empty_env("GATEWAY_IDENTITY_HEADER")?
            .unwrap_or_else(|| String::from("x-pomerium-jwt-assertion"));
        let header = HeaderName::from_str(&header_value)
            .map_err(|_| String::from("GATEWAY_IDENTITY_HEADER is invalid"))?;
        let jwks_value = non_empty_env("GATEWAY_IDENTITY_JWKS_URL")?.ok_or_else(|| {
            String::from("TRUST_GATEWAY_IDENTITY requires GATEWAY_IDENTITY_JWKS_URL")
        })?;
        let jwks_url = Url::parse(&jwks_value)
            .map_err(|_| String::from("GATEWAY_IDENTITY_JWKS_URL is invalid"))?;
        validate_jwks_url(&jwks_url)?;
        let audience = non_empty_env("GATEWAY_IDENTITY_AUDIENCE")?.ok_or_else(|| {
            String::from("TRUST_GATEWAY_IDENTITY requires GATEWAY_IDENTITY_AUDIENCE")
        })?;
        let issuer = non_empty_env("GATEWAY_IDENTITY_ISSUER")?;
        let algorithms_value =
            non_empty_env("GATEWAY_IDENTITY_ALGORITHMS")?.unwrap_or_else(|| String::from("ES256"));
        let algorithms = algorithms_value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(parse_asymmetric_algorithm)
            .collect::<Result<Vec<_>, _>>()?;
        if algorithms.is_empty() {
            return Err(String::from(
                "GATEWAY_IDENTITY_ALGORITHMS must contain an asymmetric algorithm",
            ));
        }
        let families = algorithms
            .iter()
            .map(algorithm_family)
            .collect::<HashSet<_>>();
        if families.len() != 1 {
            return Err(String::from(
                "GATEWAY_IDENTITY_ALGORITHMS must use one asymmetric algorithm family",
            ));
        }
        Ok(Self {
            header,
            jwks_url,
            algorithms,
            audience,
            issuer,
            cache: Mutex::new(None),
        })
    }
}

impl AllowedRedirect {
    fn matches(&self, candidate: &str) -> bool {
        match self {
            Self::Exact(expected) => constant_time_eq(expected.as_bytes(), candidate.as_bytes()),
            Self::LoopbackAnyPort {
                scheme,
                host,
                path,
                query,
            } => Url::parse(candidate).ok().is_some_and(|url| {
                url.scheme() == scheme
                    && normalized_host(&url) == *host
                    && url.port().is_some()
                    && url.path() == path
                    && url.query() == query.as_deref()
                    && url.fragment().is_none()
                    && url.username().is_empty()
                    && url.password().is_none()
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    pub code_challenge: String,
    #[serde(default)]
    pub code_challenge_method: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LocalAccessClaims {
    iss: String,
    aud: String,
    exp: i64,
    iat: i64,
    jti: String,
    sub: String,
    token_use: String,
    scopes: Vec<String>,
    client_id: String,
}

#[derive(Debug, Deserialize)]
struct GatewayClaims {
    email: String,
    aud: GatewayAudience,
    exp: i64,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum GatewayAudience {
    One(String),
    Many(Vec<String>),
}

impl GatewayAudience {
    fn exactly(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => constant_time_eq(value.as_bytes(), expected.as_bytes()),
            Self::Many(values) if values.len() == 1 => {
                constant_time_eq(values[0].as_bytes(), expected.as_bytes())
            }
            Self::Many(_) => false,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoogleTokenInfo {
    #[serde(default)]
    audience: Option<String>,
    #[serde(default)]
    aud: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: FlexibleInteger,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(untagged)]
enum FlexibleInteger {
    Integer(i64),
    StringValue(#[serde(deserialize_with = "parse_i64_string")] i64),
    #[default]
    Missing,
}

impl FlexibleInteger {
    fn as_i64(self) -> Option<i64> {
        match self {
            Self::Integer(value) | Self::StringValue(value) => Some(value),
            Self::Missing => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: FlexibleInteger,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfoResponse {
    email: String,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    sub: Option<String>,
}

#[derive(Debug)]
struct GoogleUserInfo {
    email: String,
    subject: Option<String>,
}

/// Construct all OAuth endpoints for attachment to a native Axum transport.
pub fn router(runtime: Arc<OAuthRuntime>) -> Router {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata_handler),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata_handler),
        )
        .route("/register", post(register_client_handler))
        .route("/authorize", get(authorize_handler))
        .route("/oauth2callback", get(oauth2_callback_handler))
        .route("/token", post(token_handler))
        .with_state(runtime)
}

pub async fn authorization_server_metadata_handler(
    State(runtime): State<Arc<OAuthRuntime>>,
) -> Response {
    no_store_json(runtime.authorization_server_metadata())
}

pub async fn protected_resource_metadata_handler(
    State(runtime): State<Arc<OAuthRuntime>>,
) -> Response {
    no_store_json(runtime.protected_resource_metadata())
}

pub async fn register_client_handler(
    State(runtime): State<Arc<OAuthRuntime>>,
    Json(request): Json<ClientRegistrationRequest>,
) -> Result<Response, AuthHttpError> {
    let response = runtime.register_client(request).await?;
    Ok((StatusCode::CREATED, Json(response)).into_response())
}

pub async fn authorize_handler(
    State(runtime): State<Arc<OAuthRuntime>>,
    Query(query): Query<AuthorizeQuery>,
) -> Result<Response, AuthHttpError> {
    let target = runtime.begin_internal_authorization(query).await?;
    Ok(Redirect::to(target.as_str()).into_response())
}

pub async fn oauth2_callback_handler(
    State(runtime): State<Arc<OAuthRuntime>>,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AuthHttpError> {
    runtime.complete_google_callback(query).await
}

pub async fn token_handler(
    State(runtime): State<Arc<OAuthRuntime>>,
    Form(request): Form<TokenRequest>,
) -> Result<Response, AuthHttpError> {
    let response = runtime.exchange_token(request).await?;
    Ok(no_store_json(response))
}

/// Begin the legacy browser grant. The returned Markdown always contains the URL.
pub fn start_google_auth(service_name: &str, expected_email: &str) -> Result<String, String> {
    let runtime = OAuthRuntime::from_env()?;
    if runtime.mode != AuthMode::Legacy && runtime.mode != AuthMode::TrustedGateway {
        return Err(String::from(
            "start_google_auth is unavailable while OAuth 2.1 bearer authentication is enabled",
        ));
    }
    let expected_email = canonical_email(expected_email)?;
    let authoritative = crate::auth::request_principal_email()
        .or_else(|| {
            runtime
                .fixed_principal
                .as_ref()
                .map(|value| value.email.clone())
        })
        .or_else(|| non_empty_env("USER_GOOGLE_EMAIL").ok().flatten())
        .ok_or_else(|| String::from("No authoritative Google principal is available"))?;
    let authoritative = canonical_email(&authoritative)?;
    if !constant_time_eq(authoritative.as_bytes(), expected_email.as_bytes()) {
        return Err(String::from(
            "The requested Google account does not match the authoritative principal",
        ));
    }
    if runtime.google_client_id.is_none() || runtime.google_client_secret.is_none() {
        return Err(String::from(
            "GOOGLE_OAUTH_CLIENT_ID and GOOGLE_OAUTH_CLIENT_SECRET are required",
        ));
    }
    let state = random_opaque(24)?;
    let verifier = random_opaque(48)?;
    let challenge = pkce_challenge(&verifier);
    let scopes = scopes_for_service(service_name, &runtime.scopes);
    let pending = PendingGoogleAuthorization {
        purpose: PendingPurpose::Legacy {
            expected_email: expected_email.clone(),
        },
        google_code_verifier: verifier,
        requested_scopes: scopes.clone(),
        expires_at: now() + STATE_TTL_SECONDS,
    };
    persist_legacy_pending(&state, pending)?;
    let prompt = LocalCredentialStore::from_env()
        .ok()
        .and_then(|store| store.load(&expected_email).ok().flatten())
        .filter(|credential| {
            credential
                .refresh_token
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        })
        .map_or("consent", |_| "select_account");
    let url = runtime.google_authorization_url(
        &state,
        &challenge,
        &scopes,
        Some(&expected_email),
        prompt,
    )?;
    let opened = open_browser_best_effort(url.as_str());
    Ok(format!(
        "Authenticate **{service_name}** as `{expected_email}` using this URL:\n\n{}\n\nBrowser opened automatically: {opened}",
        url.as_str()
    ))
}

fn open_browser_best_effort(url: &str) -> bool {
    let transport = env::var("WORKSPACE_MCP_TRANSPORT").unwrap_or_else(|_| String::from("stdio"));
    if transport != "stdio" {
        return false;
    }
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return false;

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        command.arg(url);
        command.spawn().is_ok()
    }
}

fn no_store_json<T: Serialize>(value: T) -> Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, must-revalidate"),
    );
    response
}

fn safe_html(message: &str) -> Html<String> {
    let escaped = html_escape(message);
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Google authorization</title></head><body><p>{escaped}</p></body></html>"
    ))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn require_bearer(headers: &HeaderMap) -> Result<String, AuthHttpError> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            AuthHttpError::bearer(
                StatusCode::UNAUTHORIZED,
                "invalid_request",
                "A Bearer authorization header is required",
            )
        })?;
    let (scheme, token) = value.split_once(' ').ok_or_else(|| {
        AuthHttpError::bearer(
            StatusCode::UNAUTHORIZED,
            "invalid_request",
            "A Bearer authorization header is required",
        )
    })?;
    if !scheme.eq_ignore_ascii_case("Bearer") || token.trim().is_empty() {
        return Err(AuthHttpError::bearer(
            StatusCode::UNAUTHORIZED,
            "invalid_request",
            "A Bearer authorization header is required",
        ));
    }
    Ok(token.trim().to_owned())
}

async fn bounded_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, AuthHttpError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_BODY as u64)
    {
        return Err(AuthHttpError::server_error());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| AuthHttpError::server_error())?;
    if bytes.len() > MAX_HTTP_BODY {
        return Err(AuthHttpError::server_error());
    }
    serde_json::from_slice(&bytes).map_err(|_| AuthHttpError::server_error())
}

fn parse_allowed_redirects(value: &str) -> Result<Vec<AllowedRedirect>, String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !seen.insert(item.to_owned()) {
            continue;
        }
        if item.contains('*') {
            result.push(parse_loopback_wildcard(item)?);
            continue;
        }
        let url =
            Url::parse(item).map_err(|_| format!("Invalid allowed client redirect URI: {item}"))?;
        validate_redirect_url(&url)?;
        result.push(AllowedRedirect::Exact(item.to_owned()));
    }
    if result.is_empty() {
        return Err(String::from(
            "WORKSPACE_MCP_ALLOWED_CLIENT_REDIRECT_URIS must not be empty",
        ));
    }
    Ok(result)
}

fn parse_loopback_wildcard(value: &str) -> Result<AllowedRedirect, String> {
    if value.matches('*').count() != 1 {
        return Err(String::from(
            "Only one loopback port wildcard is permitted in a redirect URI",
        ));
    }
    let marker = ":*";
    let index = value.find(marker).ok_or_else(|| {
        String::from("A redirect wildcard is permitted only as the loopback port")
    })?;
    let synthetic = format!("{}:1{}", &value[..index], &value[index + marker.len()..]);
    let url =
        Url::parse(&synthetic).map_err(|_| String::from("The loopback redirect URI is invalid"))?;
    if url.scheme() != "http" || !is_loopback_url(&url) || url.fragment().is_some() {
        return Err(String::from(
            "A wildcard port is allowed only for an HTTP loopback redirect URI",
        ));
    }
    Ok(AllowedRedirect::LoopbackAnyPort {
        scheme: String::from("http"),
        host: normalized_host(&url),
        path: url.path().to_owned(),
        query: url.query().map(str::to_owned),
    })
}

fn validate_redirect_url(url: &Url) -> Result<(), String> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(String::from(
            "Client redirect URIs must not contain userinfo or fragments",
        ));
    }
    if is_loopback_url(url) {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(String::from(
                "Loopback redirect URIs must use HTTP or HTTPS",
            ));
        }
    } else if url.scheme() != "https" {
        return Err(String::from("Remote client redirect URIs must use HTTPS"));
    }
    Ok(())
}

fn validate_jwks_url(url: &Url) -> Result<(), String> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(String::from(
            "GATEWAY_IDENTITY_JWKS_URL must not contain credentials or a fragment",
        ));
    }
    if url.scheme() == "https" || (url.scheme() == "http" && is_loopback_url(url)) {
        Ok(())
    } else {
        Err(String::from(
            "GATEWAY_IDENTITY_JWKS_URL must use HTTPS; HTTP is allowed only for loopback",
        ))
    }
}

async fn fetch_jwks(url: &Url) -> Result<Vec<u8>, String> {
    if is_loopback_url(url) {
        let mut response = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| String::from("Failed to initialize loopback JWKS client"))?
            .get(url.clone())
            .send()
            .await
            .map_err(|_| String::from("Failed to fetch loopback JWKS"))?;
        if !response.status().is_success() {
            return Err(String::from("Loopback JWKS endpoint returned an error"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_HTTP_BODY as u64)
        {
            return Err(String::from("JWKS response is too large"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| String::from("Failed to read loopback JWKS"))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_HTTP_BODY {
                return Err(String::from("JWKS response is too large"));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(body);
    }
    let fetched = SafeFetcher::new(MAX_HTTP_BODY, HTTP_TIMEOUT)
        .with_max_redirects(0)
        .fetch(url.as_str())
        .await?;
    if !fetched.status.is_success() || fetched.final_url.scheme() != "https" {
        return Err(String::from("JWKS endpoint returned an error"));
    }
    Ok(fetched.body)
}

fn effective_public_url() -> Result<Url, String> {
    let value = if let Some(external) = non_empty_env("WORKSPACE_EXTERNAL_URL")? {
        external
    } else {
        let base =
            env::var("WORKSPACE_MCP_BASE_URI").unwrap_or_else(|_| String::from("http://localhost"));
        let resolved = env::var("WORKSPACE_MCP_RESOLVED_PORT").ok().as_deref() == Some("1");
        let port = if resolved {
            env::var("WORKSPACE_MCP_PORT")
                .or_else(|_| env::var("PORT"))
                .unwrap_or_else(|_| String::from("8000"))
        } else {
            env::var("PORT")
                .or_else(|_| env::var("WORKSPACE_MCP_PORT"))
                .unwrap_or_else(|_| String::from("8000"))
        };
        let base = base.trim_end_matches('/');
        if Url::parse(base).ok().and_then(|url| url.port()).is_some() {
            base.to_owned()
        } else {
            format!("{base}:{port}")
        }
    };
    let mut url = Url::parse(&value)
        .map_err(|_| String::from("The OAuth public URL must be an absolute HTTP(S) URL"))?;
    require_secure_http_url(&url, "OAuth public URL")?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(String::from(
            "The OAuth public URL must not contain credentials or a fragment",
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn require_secure_http_url(url: &Url, name: &str) -> Result<(), String> {
    if url.host_str().is_none() {
        return Err(format!("{name} must be an absolute HTTP(S) URL"));
    }
    if url.scheme() == "https" || (url.scheme() == "http" && is_loopback_url(url)) {
        Ok(())
    } else {
        Err(format!(
            "{name} must use HTTPS; HTTP is allowed only for loopback"
        ))
    }
}

fn is_loopback_url(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
    })
}

fn normalized_host(url: &Url) -> String {
    url.host_str().unwrap_or_default().to_ascii_lowercase()
}

fn configured_scopes() -> Vec<String> {
    non_empty_env("WORKSPACE_MCP_OAUTH_SCOPES")
        .ok()
        .flatten()
        .map(|value| split_scopes(&value))
        .filter(|scopes| !scopes.is_empty())
        .unwrap_or_else(|| split_scopes(ALL_GOOGLE_SCOPES))
}

fn scopes_for_service(service_name: &str, configured: &[String]) -> Vec<String> {
    let mut scopes = configured.to_vec();
    let service = service_name.trim().to_ascii_lowercase();
    let additional = match service.as_str() {
        "gmail" => Some("https://www.googleapis.com/auth/gmail.modify"),
        "drive" => Some("https://www.googleapis.com/auth/drive"),
        "calendar" => Some("https://www.googleapis.com/auth/calendar"),
        "docs" | "documents" => Some("https://www.googleapis.com/auth/documents"),
        "sheets" | "spreadsheets" => Some("https://www.googleapis.com/auth/spreadsheets"),
        "slides" | "presentations" => Some("https://www.googleapis.com/auth/presentations"),
        "tasks" => Some("https://www.googleapis.com/auth/tasks"),
        "contacts" => Some("https://www.googleapis.com/auth/contacts"),
        "chat" => Some("https://www.googleapis.com/auth/chat.messages"),
        "forms" => Some("https://www.googleapis.com/auth/forms.body"),
        "appscript" | "apps script" => Some("https://www.googleapis.com/auth/script.projects"),
        _ => None,
    };
    if let Some(scope) = additional
        && !scopes.iter().any(|existing| existing == scope)
    {
        scopes.push(scope.to_owned());
    }
    scopes
}

fn has_protocol_scopes(scopes: &[String]) -> bool {
    ["openid", "https://www.googleapis.com/auth/userinfo.email"]
        .iter()
        .all(|required| scopes.iter().any(|scope| scope == required))
}

fn split_scopes(value: &str) -> Vec<String> {
    let mut scopes = value
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    scopes
}

fn configured_domains() -> Vec<String> {
    env::var("DWD_ALLOWED_DOMAINS")
        .ok()
        .map(|value| split_scopes(&value))
        .unwrap_or_default()
}

fn parse_asymmetric_algorithm(value: &str) -> Result<Algorithm, String> {
    match value {
        "ES256" => Ok(Algorithm::ES256),
        "ES384" => Ok(Algorithm::ES384),
        "EdDSA" => Ok(Algorithm::EdDSA),
        "PS256" => Ok(Algorithm::PS256),
        "PS384" => Ok(Algorithm::PS384),
        "PS512" => Ok(Algorithm::PS512),
        "RS256" => Ok(Algorithm::RS256),
        "RS384" => Ok(Algorithm::RS384),
        "RS512" => Ok(Algorithm::RS512),
        _ => Err(String::from(
            "GATEWAY_IDENTITY_ALGORITHMS must contain only supported asymmetric JWT algorithms",
        )),
    }
}

fn algorithm_family(algorithm: &Algorithm) -> &'static str {
    match algorithm {
        Algorithm::ES256 | Algorithm::ES384 => "EC",
        Algorithm::EdDSA => "EdDSA",
        Algorithm::PS256 | Algorithm::PS384 | Algorithm::PS512 => "PS",
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => "RS",
        _ => "symmetric",
    }
}

fn valid_pkce_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_pkce_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_opaque(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| String::from("Secure random generation failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn non_empty_env(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value.trim().to_owned())),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(format!("{name} is not valid Unicode")),
    }
}

fn env_bool(name: &str) -> Result<Option<bool>, String> {
    match non_empty_env(name)? {
        None => Ok(None),
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(Some(true)),
            "false" | "0" | "no" => Ok(Some(false)),
            _ => Err(format!("{name} must be true or false")),
        },
    }
}

fn positive_usize_env(name: &str, default: usize) -> Result<usize, String> {
    match non_empty_env(name)? {
        None => Ok(default),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|parsed| *parsed > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
    }
}

fn positive_i64_env(name: &str, default: i64) -> Result<i64, String> {
    match non_empty_env(name)? {
        None => Ok(default),
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|parsed| *parsed > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
    }
}

fn parse_i64_string<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer)?
        .parse::<i64>()
        .map_err(serde::de::Error::custom)
}

static LEGACY_PENDING: OnceLock<Mutex<HashMap<String, PendingGoogleAuthorization>>> =
    OnceLock::new();

fn legacy_pending_store() -> &'static Mutex<HashMap<String, PendingGoogleAuthorization>> {
    LEGACY_PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn persist_legacy_pending(state: &str, pending: PendingGoogleAuthorization) -> Result<(), String> {
    update_legacy_state_file(|entries| {
        entries.insert(state.to_owned(), pending.clone());
        Ok(())
    })?;
    legacy_pending_store()
        .try_lock()
        .map_err(|_| String::from("Another authorization request is being initialized"))?
        .insert(state.to_owned(), pending);
    Ok(())
}

fn take_legacy_pending(state: &str) -> Option<PendingGoogleAuthorization> {
    let persisted = update_legacy_state_file(|entries| Ok(entries.remove(state))).ok()?;
    if let Ok(mut memory) = legacy_pending_store().try_lock() {
        memory.remove(state);
    }
    persisted
}

fn update_legacy_state_file<T>(
    update: impl FnOnce(&mut HashMap<String, PendingGoogleAuthorization>) -> Result<T, String>,
) -> Result<T, String> {
    let store = LocalCredentialStore::from_env()?;
    let directory = store.directory();
    let state_path = directory.join("oauth_states.json");
    let lock_path = directory.join(".oauth_states.lock");
    let mut lock_options = OpenOptions::new();
    lock_options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        lock_options.mode(0o600);
    }
    let lock = lock_options
        .open(&lock_path)
        .map_err(|_| String::from("Failed to open OAuth state lock"))?;
    lock.lock_exclusive()
        .map_err(|_| String::from("Failed to lock OAuth state store"))?;
    secure_private_file(&lock_path)?;

    let mut entries = read_legacy_states(&state_path)?;
    let current = now();
    entries.retain(|_, pending| pending.expires_at > current);
    let result = update(&mut entries);
    if result.is_ok() {
        write_legacy_states(directory, &state_path, &entries)?;
    }
    let _ = FileExt::unlock(&lock);
    result
}

fn read_legacy_states(path: &Path) -> Result<HashMap<String, PendingGoogleAuthorization>, String> {
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let mut content = Vec::new();
    OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|mut file| file.read_to_end(&mut content))
        .map_err(|_| String::from("Failed to read OAuth state store"))?;
    if content.is_empty() {
        return Ok(HashMap::new());
    }
    serde_json::from_slice(&content).map_err(|_| String::from("OAuth state store is invalid"))
}

fn write_legacy_states(
    directory: &Path,
    path: &Path,
    entries: &HashMap<String, PendingGoogleAuthorization>,
) -> Result<(), String> {
    let suffix = random_opaque(12)?;
    let temporary = directory.join(format!(".oauth_states.{suffix}.tmp"));
    let bytes = serde_json::to_vec(entries)
        .map_err(|_| String::from("Failed to serialize OAuth state store"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<(), String> {
        let mut file = options
            .open(&temporary)
            .map_err(|_| String::from("Failed to create OAuth state file"))?;
        file.write_all(&bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|_| String::from("Failed to persist OAuth state"))?;
        fs::rename(&temporary, path)
            .map_err(|_| String::from("Failed to replace OAuth state store"))?;
        secure_private_file(path)?;
        if let Ok(directory_file) = fs::File::open(directory) {
            let _ = directory_file.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn secure_private_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| String::from("Failed to secure OAuth state file"))?;
    }
    Ok(())
}
