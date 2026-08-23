//! Native stdio and Streamable HTTP transport adapters.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::env;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, ORIGIN,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, oneshot, watch};
use tokio::task::JoinHandle;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use url::Url;
use uuid::Uuid;

use crate::auth::{RequestAuthContext, with_request_auth};
use crate::config::Config;
use crate::mcp::{McpEngine, McpMode, McpSessionState};
use crate::native::common::attachments::{AttachmentStore, global_attachment_store};
use crate::oauth::{self, AuthMode, OAuthRuntime, VerifiedRequest};

const DEFAULT_HTTP_PORT: u16 = 8_000;
const DEFAULT_BODY_LIMIT: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_SESSIONS: usize = 4_096;
const DEFAULT_MAX_SESSIONS_PER_PRINCIPAL: usize = 64;
const DEFAULT_SESSION_IDLE_TTL_SECONDS: u64 = 1_800;
const SESSION_HEADER: &str = "mcp-session-id";
const BODY_LIMIT_ENVIRONMENTS: [&str; 4] = [
    "WORKSPACE_MCP_MAX_BODY_BYTES",
    "WORKSPACE_MCP_HTTP_BODY_LIMIT",
    "WORKSPACE_MCP_BODY_LIMIT",
    "WORKSPACE_MCP_MAX_REQUEST_SIZE",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransportKind {
    Stdio,
    StreamableHttp,
}

#[derive(Clone)]
struct SessionRecord {
    subject: String,
    state: Arc<McpSessionState>,
    created_at: Instant,
    last_seen: Instant,
}

#[derive(Clone)]
struct HttpState {
    engine: Arc<McpEngine>,
    runtime: Arc<OAuthRuntime>,
    sessions: Arc<RwLock<HashMap<String, SessionRecord>>>,
    attachments: Option<Arc<AttachmentStore>>,
    body_limit: usize,
    max_sessions: usize,
    max_sessions_per_principal: usize,
    session_idle_ttl: Duration,
    shutdown: watch::Receiver<bool>,
}

#[derive(Clone)]
struct AllowedOrigins {
    explicit: Arc<HashSet<String>>,
}

impl AllowedOrigins {
    fn from_runtime(runtime: &OAuthRuntime) -> Result<Self, String> {
        let mut explicit = HashSet::new();
        explicit.insert(origin_of_url(runtime.public_url()));
        if let Some(configured) = optional_env("WORKSPACE_MCP_ALLOWED_ORIGINS")? {
            for value in configured
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                explicit.insert(normalize_origin(value)?);
            }
        }
        Ok(Self {
            explicit: Arc::new(explicit),
        })
    }

    fn permits(&self, value: &HeaderValue) -> bool {
        let Ok(value) = value.to_str() else {
            return false;
        };
        let Ok(url) = Url::parse(value) else {
            return false;
        };
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        if url.host_str().is_some_and(is_loopback_host) {
            return true;
        }
        self.explicit.contains(&url.origin().ascii_serialization())
    }
}

struct CallbackServer {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<(), String>>,
}

impl CallbackServer {
    async fn stop(self) -> Result<(), String> {
        let _ = self.shutdown.send(());
        self.task
            .await
            .map_err(|error| format!("OAuth callback server task failed: {error}"))?
    }
}

/// Run the native MCP server using the configured stdio or Streamable HTTP transport.
pub async fn run(config: &Config, args: &[String]) -> Result<(), String> {
    let transport = selected_transport(args)?;
    let runtime = OAuthRuntime::from_env()?;
    let mode = match runtime.mode() {
        AuthMode::Legacy => McpMode::Legacy,
        AuthMode::InternalOAuth21 | AuthMode::ExternalGoogle => McpMode::Oauth21,
        AuthMode::TrustedGateway => McpMode::Gateway,
    };
    let selection = config.effective_native_selection(args)?;
    let engine = Arc::new(
        McpEngine::new(
            mode,
            &selection.tools,
            selection.restrict_tasks_manage_actions,
        )
        .map_err(|error| format!("failed to initialize MCP engine: {error}"))?,
    );

    match transport {
        TransportKind::Stdio => run_stdio(engine, runtime).await,
        TransportKind::StreamableHttp => run_http(engine, runtime).await,
    }
}

fn selected_transport(args: &[String]) -> Result<TransportKind, String> {
    let mut selected = None;
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "--transport" {
            let value = args
                .get(index + 1)
                .filter(|value| !value.starts_with('-'))
                .ok_or_else(|| String::from("--transport requires stdio or streamable-http"))?;
            selected = Some(value.clone());
            index += 1;
        } else if let Some(value) = argument.strip_prefix("--transport=") {
            if value.is_empty() {
                return Err(String::from(
                    "--transport requires stdio or streamable-http",
                ));
            }
            selected = Some(value.to_owned());
        }
        index += 1;
    }
    let value = match selected {
        Some(value) => value,
        None => env::var("WORKSPACE_MCP_TRANSPORT").unwrap_or_else(|_| String::from("stdio")),
    };
    match value.as_str() {
        "stdio" => Ok(TransportKind::Stdio),
        "streamable-http" => Ok(TransportKind::StreamableHttp),
        _ => Err(format!(
            "unsupported transport '{value}'; expected stdio or streamable-http"
        )),
    }
}

async fn run_stdio(engine: Arc<McpEngine>, runtime: Arc<OAuthRuntime>) -> Result<(), String> {
    if runtime.mode() != AuthMode::Legacy {
        return Err(String::from(
            "stdio transport supports only legacy fixed-principal authentication (including service-account delegation); internal OAuth, external OAuth, and trusted gateway authentication require per-request HTTP headers",
        ));
    }
    let verified = runtime
        .authenticate_headers(&HeaderMap::new())
        .await
        .map_err(|error| {
            format!(
                "failed to authenticate the fixed stdio principal: {}",
                error.json_body()
            )
        })?;
    let auth = request_auth_context(&verified);
    let callback = start_stdio_callback_if_needed(Arc::clone(&runtime)).await?;

    let result = tokio::select! {
        result = stdio_loop(engine, auth) => result,
        () = shutdown_signal() => Ok(()),
    };
    let callback_result = match callback {
        Some(server) => server.stop().await,
        None => Ok(()),
    };
    result.and(callback_result)
}

async fn stdio_loop(engine: Arc<McpEngine>, auth: RequestAuthContext) -> Result<(), String> {
    let session = McpSessionState::default();
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut line = Vec::new();

    loop {
        line.clear();
        let read = input
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| format!("failed to read stdin: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line.pop();
        }
        let response = with_request_auth(auth.clone(), engine.handle_bytes(&session, &line)).await;
        if let Some(response) = response {
            let mut encoded = serde_json::to_vec(&response)
                .map_err(|error| format!("failed to serialize MCP response: {error}"))?;
            encoded.push(b'\n');
            output
                .write_all(&encoded)
                .await
                .map_err(|error| format!("failed to write stdout: {error}"))?;
            output
                .flush()
                .await
                .map_err(|error| format!("failed to flush stdout: {error}"))?;
        }
    }
}

async fn start_stdio_callback_if_needed(
    runtime: Arc<OAuthRuntime>,
) -> Result<Option<CallbackServer>, String> {
    if runtime.mode() != AuthMode::Legacy
        || optional_env("GOOGLE_OAUTH_CLIENT_ID")?.is_none()
        || optional_env("GOOGLE_OAUTH_CLIENT_SECRET")?.is_none()
    {
        return Ok(None);
    }

    let redirect = match optional_env("GOOGLE_OAUTH_REDIRECT_URI")? {
        Some(value) => Url::parse(&value)
            .map_err(|_| String::from("GOOGLE_OAUTH_REDIRECT_URI must be an absolute URL"))?,
        None => runtime
            .public_url()
            .join("oauth2callback")
            .map_err(|_| String::from("failed to construct the OAuth callback URL"))?,
    };
    if redirect.scheme() != "http"
        || !redirect.host_str().is_some_and(is_loopback_host)
        || redirect.port_or_known_default().is_none()
    {
        return Err(String::from(
            "stdio OAuth callback requires an http:// loopback GOOGLE_OAUTH_REDIRECT_URI with a port",
        ));
    }
    let host = redirect.host_str().unwrap_or("127.0.0.1");
    let port = redirect
        .port_or_known_default()
        .unwrap_or(DEFAULT_HTTP_PORT);
    let bind = socket_address(host, port);
    let listener = TcpListener::bind(&bind).await.map_err(|error| {
        format!("failed to bind stdio OAuth callback server on {bind}: {error}")
    })?;

    let callback_path = redirect.path();
    let mut router = Router::new().route("/oauth2callback", get(oauth::oauth2_callback_handler));
    if callback_path != "/oauth2callback" {
        router = router.route(callback_path, get(oauth::oauth2_callback_handler));
    }
    let router = router.with_state(runtime);
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = receiver.await;
            })
            .await
            .map_err(|error| format!("stdio OAuth callback server failed: {error}"))
    });
    eprintln!("workspace-mcp-rs: OAuth callback listening on {bind}{callback_path}");
    Ok(Some(CallbackServer { shutdown, task }))
}

async fn run_http(engine: Arc<McpEngine>, runtime: Arc<OAuthRuntime>) -> Result<(), String> {
    let attachments = if runtime.stateless() {
        None
    } else {
        Some(global_attachment_store()?)
    };
    let body_limit = body_limit()?;
    let max_sessions = positive_usize_env("WORKSPACE_MCP_MAX_SESSIONS", DEFAULT_MAX_SESSIONS)?;
    let max_sessions_per_principal = positive_usize_env(
        "WORKSPACE_MCP_MAX_SESSIONS_PER_PRINCIPAL",
        DEFAULT_MAX_SESSIONS_PER_PRINCIPAL,
    )?;
    if max_sessions_per_principal > max_sessions {
        return Err(String::from(
            "WORKSPACE_MCP_MAX_SESSIONS_PER_PRINCIPAL must not exceed WORKSPACE_MCP_MAX_SESSIONS",
        ));
    }
    let session_idle_ttl = Duration::from_secs(positive_u64_env(
        "WORKSPACE_MCP_SESSION_IDLE_TTL_SECONDS",
        DEFAULT_SESSION_IDLE_TTL_SECONDS,
    )?);
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let state = HttpState {
        engine,
        runtime: Arc::clone(&runtime),
        sessions: Arc::new(RwLock::new(HashMap::new())),
        attachments,
        body_limit,
        max_sessions,
        max_sessions_per_principal,
        session_idle_ttl,
        shutdown: shutdown_receiver,
    };
    let allowed_origins = AllowedOrigins::from_runtime(&runtime)?;
    let cors_origins = allowed_origins.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _parts| {
            cors_origins.permits(origin)
        }))
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers(cors_allowed_headers()?)
        .expose_headers([HeaderName::from_static(SESSION_HEADER)])
        .allow_credentials(true);

    let application = Router::new()
        .route("/", get(health_handler))
        .route("/health", get(health_handler))
        .route(
            "/mcp",
            post(mcp_post_handler)
                .get(mcp_get_handler)
                .delete(mcp_delete_handler),
        )
        .route("/attachments/{file_id}", get(attachment_handler))
        .with_state(state)
        .merge(oauth_routes(Arc::clone(&runtime))?)
        .layer(RequestBodyLimitLayer::new(body_limit))
        .layer(cors)
        .layer(middleware::from_fn_with_state(
            allowed_origins,
            validate_origin,
        ));

    let host = optional_env("WORKSPACE_MCP_HOST")?.unwrap_or_else(|| match runtime.mode() {
        AuthMode::Legacy => String::from("127.0.0.1"),
        AuthMode::InternalOAuth21 | AuthMode::ExternalGoogle | AuthMode::TrustedGateway => {
            String::from("0.0.0.0")
        }
    });
    let port = http_port()?;
    let bind = socket_address(&host, port);
    let listener = TcpListener::bind(&bind)
        .await
        .map_err(|error| format!("failed to bind Streamable HTTP server on {bind}: {error}"))?;
    eprintln!("workspace-mcp-rs: Streamable HTTP listening on {bind}");
    axum::serve(listener, application)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_sender.send(true);
        })
        .await
        .map_err(|error| format!("Streamable HTTP server failed: {error}"))
}

fn oauth_routes(runtime: Arc<OAuthRuntime>) -> Result<Router, String> {
    let callback_enabled = runtime.mode() == AuthMode::InternalOAuth21 || callback_route_needed()?;
    let callback_path = runtime.google_callback_path().to_owned();
    let mut router = match runtime.mode() {
        AuthMode::InternalOAuth21 => oauth::router(Arc::clone(&runtime)),
        AuthMode::ExternalGoogle => Router::new()
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                get(oauth::protected_resource_metadata_handler),
            )
            .with_state(Arc::clone(&runtime)),
        AuthMode::Legacy | AuthMode::TrustedGateway if callback_enabled => Router::new()
            .route("/oauth2callback", get(oauth::oauth2_callback_handler))
            .with_state(Arc::clone(&runtime)),
        AuthMode::Legacy | AuthMode::TrustedGateway => Router::new(),
    };
    if callback_enabled && callback_path != "/oauth2callback" {
        router = router.merge(
            Router::new()
                .route(&callback_path, get(oauth::oauth2_callback_handler))
                .with_state(runtime),
        );
    }
    Ok(router)
}

fn callback_route_needed() -> Result<bool, String> {
    Ok(optional_env("GOOGLE_OAUTH_CLIENT_ID")?.is_some()
        && optional_env("GOOGLE_OAUTH_CLIENT_SECRET")?.is_some())
}

async fn health_handler() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "transport": "streamable-http"
    }))
}

async fn mcp_post_handler(State(state): State<HttpState>, request: Request) -> Response {
    let headers = request.headers().clone();
    let verified = match state.runtime.authenticate_headers(&headers).await {
        Ok(verified) => verified,
        Err(error) => return state.runtime.error_response(error),
    };
    let body = match axum::body::to_bytes(request.into_body(), state.body_limit).await {
        Ok(body) => body,
        Err(_) => {
            return json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "The MCP request body exceeds the configured limit",
            );
        }
    };
    let parsed = serde_json::from_slice::<Value>(&body);
    if state.runtime.stateless() {
        return handle_stateless_post(&state, &verified, &body, parsed.ok()).await;
    }
    handle_stateful_post(&state, &verified, &headers, &body, parsed.ok()).await
}

async fn handle_stateless_post(
    state: &HttpState,
    verified: &VerifiedRequest,
    body: &[u8],
    request: Option<Value>,
) -> Response {
    let session = match request
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
    {
        Some("initialize") => McpSessionState::default(),
        Some("notifications/initialized") => McpSessionState::stateless_initialize_pending(),
        _ => McpSessionState::stateless_ready(),
    };
    let auth = request_auth_context(verified);
    let response = match request {
        Some(request) => {
            with_request_auth(auth, state.engine.handle_value(&session, request)).await
        }
        None => with_request_auth(auth, state.engine.handle_bytes(&session, body)).await,
    };
    mcp_response(response)
}

async fn handle_stateful_post(
    state: &HttpState,
    verified: &VerifiedRequest,
    headers: &HeaderMap,
    body: &[u8],
    request: Option<Value>,
) -> Response {
    let session_id = match session_id(headers) {
        Ok(value) => value,
        Err(error) => return session_error_response(error),
    };
    let is_initialize = request
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        == Some("initialize");

    if session_id.is_none() && is_initialize {
        let session = Arc::new(McpSessionState::default());
        let auth = request_auth_context(verified);
        let response = match request {
            Some(request) => {
                with_request_auth(auth, state.engine.handle_value(&session, request)).await
            }
            None => with_request_auth(auth, state.engine.handle_bytes(&session, body)).await,
        };
        let successful = response
            .as_ref()
            .and_then(Value::as_object)
            .is_some_and(|object| object.contains_key("result"));
        if !successful {
            return mcp_response(response);
        }
        let new_id = match insert_session(state, verified.principal.subject.clone(), session).await
        {
            Ok(id) => id,
            Err(SessionCapacityError) => return session_capacity_error(),
        };
        return mcp_response_with_session(response, &new_id);
    }

    let Some(session_id) = session_id else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "missing_session",
            "Mcp-Session-Id is required",
        );
    };
    let session = touch_session(state, &session_id, &verified.principal.subject).await;
    let Some(session) = session else {
        return unknown_session();
    };
    let auth = request_auth_context(verified);
    let response = match request {
        Some(request) => {
            with_request_auth(auth, state.engine.handle_value(&session, request)).await
        }
        None => with_request_auth(auth, state.engine.handle_bytes(&session, body)).await,
    };
    mcp_response(response)
}

struct SessionCapacityError;

async fn insert_session(
    state: &HttpState,
    subject: String,
    session: Arc<McpSessionState>,
) -> Result<String, SessionCapacityError> {
    let now = Instant::now();
    let mut sessions = state.sessions.write().await;
    prune_expired_sessions(&mut sessions, now, state.session_idle_ttl);
    let principal_sessions = sessions
        .values()
        .filter(|record| record.subject == subject)
        .count();
    if sessions.len() >= state.max_sessions
        || principal_sessions >= state.max_sessions_per_principal
    {
        return Err(SessionCapacityError);
    }
    loop {
        let id = Uuid::new_v4().to_string();
        if !sessions.contains_key(&id) {
            sessions.insert(
                id.clone(),
                SessionRecord {
                    subject: subject.clone(),
                    state: Arc::clone(&session),
                    created_at: now,
                    last_seen: now,
                },
            );
            return Ok(id);
        }
    }
}

async fn touch_session(state: &HttpState, id: &str, subject: &str) -> Option<Arc<McpSessionState>> {
    let now = Instant::now();
    let mut sessions = state.sessions.write().await;
    prune_expired_sessions(&mut sessions, now, state.session_idle_ttl);
    let record = sessions.get_mut(id)?;
    if record.subject != subject {
        return None;
    }
    record.last_seen = now;
    Some(Arc::clone(&record.state))
}

fn prune_expired_sessions(
    sessions: &mut HashMap<String, SessionRecord>,
    now: Instant,
    idle_ttl: Duration,
) {
    sessions.retain(|_, record| {
        record.created_at <= record.last_seen && now.duration_since(record.last_seen) < idle_ttl
    });
}

async fn mcp_get_handler(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let verified = match state.runtime.authenticate_headers(&headers).await {
        Ok(verified) => verified,
        Err(error) => return state.runtime.error_response(error),
    };
    if !state.runtime.stateless() {
        let id = match required_session_id(&headers) {
            Ok(id) => id,
            Err(error) => return session_error_response(error),
        };
        if touch_session(&state, &id, &verified.principal.subject)
            .await
            .is_none()
        {
            return unknown_session();
        }
    }

    let events = stream::unfold(state.shutdown.clone(), |mut shutdown| async move {
        let stopping = *shutdown.borrow();
        if !stopping {
            let _ = shutdown.changed().await;
        }
        None::<(Result<Event, Infallible>, watch::Receiver<bool>)>
    });
    Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response()
}

async fn mcp_delete_handler(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let verified = match state.runtime.authenticate_headers(&headers).await {
        Ok(verified) => verified,
        Err(error) => return state.runtime.error_response(error),
    };
    if state.runtime.stateless() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let id = match required_session_id(&headers) {
        Ok(id) => id,
        Err(error) => return session_error_response(error),
    };
    let removed = {
        let now = Instant::now();
        let mut sessions = state.sessions.write().await;
        prune_expired_sessions(&mut sessions, now, state.session_idle_ttl);
        if sessions
            .get(&id)
            .is_some_and(|record| record.subject == verified.principal.subject)
        {
            sessions.remove(&id);
            true
        } else {
            false
        }
    };
    if removed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        unknown_session()
    }
}

async fn attachment_handler(
    State(state): State<HttpState>,
    Path(file_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let verified = match state.runtime.authenticate_headers(&headers).await {
        Ok(verified) => verified,
        Err(error) => return state.runtime.error_response(error),
    };
    let Some(store) = state.attachments.as_ref() else {
        return attachment_not_found();
    };
    let Some(metadata) = store.lookup_owned_compat(&file_id, &verified.principal.email) else {
        return attachment_not_found();
    };
    let path = metadata.path.clone();
    let bytes = match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) | Err(_) => return attachment_not_found(),
    };

    let content_type = match HeaderValue::from_str(&metadata.mime_type) {
        Ok(value) => value,
        Err(_) => return internal_error(),
    };
    let disposition = format!("attachment; filename=\"{}\"", metadata.filename);
    let content_disposition = match HeaderValue::from_str(&disposition) {
        Ok(value) => value,
        Err(_) => return internal_error(),
    };
    let content_length = match HeaderValue::from_str(&bytes.len().to_string()) {
        Ok(value) => value,
        Err(_) => return internal_error(),
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
        .headers_mut()
        .insert(CONTENT_DISPOSITION, content_disposition);
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, content_length);
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn request_auth_context(verified: &VerifiedRequest) -> RequestAuthContext {
    RequestAuthContext {
        principal_email: verified.principal.email.clone(),
        google_access_token: verified.google_access_token.clone(),
        granted_scopes: verified.granted_scopes.clone(),
    }
}

fn mcp_response(response: Option<Value>) -> Response {
    match response {
        Some(value) => Json(value).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn mcp_response_with_session(response: Option<Value>, session_id: &str) -> Response {
    let mut response = mcp_response(response);
    if let Ok(value) = HeaderValue::from_str(session_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(SESSION_HEADER), value);
    }
    response
}

#[derive(Clone, Copy)]
enum SessionError {
    Missing,
    Invalid,
}

fn session_id(headers: &HeaderMap) -> Result<Option<String>, SessionError> {
    let Some(value) = headers.get(HeaderName::from_static(SESSION_HEADER)) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| SessionError::Invalid)?;
    if value.is_empty() {
        return Err(SessionError::Invalid);
    }
    Ok(Some(value.to_owned()))
}

fn required_session_id(headers: &HeaderMap) -> Result<String, SessionError> {
    session_id(headers)?.ok_or(SessionError::Missing)
}

fn session_error_response(error: SessionError) -> Response {
    match error {
        SessionError::Missing => json_error(
            StatusCode::BAD_REQUEST,
            "missing_session",
            "Mcp-Session-Id is required",
        ),
        SessionError::Invalid => json_error(
            StatusCode::BAD_REQUEST,
            "invalid_session",
            "Mcp-Session-Id is invalid",
        ),
    }
}

fn unknown_session() -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "session_not_found",
        "MCP session was not found",
    )
}

fn session_capacity_error() -> Response {
    json_error(
        StatusCode::TOO_MANY_REQUESTS,
        "session_capacity_reached",
        "The MCP session capacity has been reached",
    )
}

fn attachment_not_found() -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "attachment_not_found",
        "Attachment was not found",
    )
}

fn internal_error() -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "The server could not complete the request",
    )
}

fn json_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(json!({ "error": code, "error_description": message })),
    )
        .into_response()
}

async fn validate_origin(
    State(allowed): State<AllowedOrigins>,
    request: Request,
    next: Next,
) -> Response {
    if request
        .headers()
        .get(ORIGIN)
        .is_some_and(|origin| !allowed.permits(origin))
    {
        return json_error(
            StatusCode::FORBIDDEN,
            "origin_not_allowed",
            "The request origin is not allowed",
        );
    }
    next.run(request).await
}

fn cors_allowed_headers() -> Result<Vec<HeaderName>, String> {
    let mut headers = vec![
        ACCEPT,
        AUTHORIZATION,
        CONTENT_TYPE,
        HeaderName::from_static(SESSION_HEADER),
    ];
    if let Some(value) = optional_env("GATEWAY_IDENTITY_HEADER")? {
        let header = HeaderName::from_str(&value)
            .map_err(|_| String::from("GATEWAY_IDENTITY_HEADER is invalid"))?;
        if !headers.contains(&header) {
            headers.push(header);
        }
    } else {
        headers.push(HeaderName::from_static("x-pomerium-jwt-assertion"));
    }
    Ok(headers)
}

fn body_limit() -> Result<usize, String> {
    for name in BODY_LIMIT_ENVIRONMENTS {
        if let Some(value) = optional_env(name)? {
            return value
                .parse::<usize>()
                .map_err(|_| format!("{name} must be a positive integer number of bytes"))
                .and_then(|limit| {
                    if limit == 0 {
                        Err(format!("{name} must be a positive integer number of bytes"))
                    } else {
                        Ok(limit)
                    }
                });
        }
    }
    Ok(DEFAULT_BODY_LIMIT)
}

fn positive_usize_env(name: &str, default: usize) -> Result<usize, String> {
    match optional_env(name)? {
        None => Ok(default),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|parsed| *parsed > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
    }
}

fn positive_u64_env(name: &str, default: u64) -> Result<u64, String> {
    match optional_env(name)? {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|parsed| *parsed > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
    }
}

fn http_port() -> Result<u16, String> {
    let value = match optional_env("PORT")? {
        Some(value) => value,
        None => {
            optional_env("WORKSPACE_MCP_PORT")?.unwrap_or_else(|| DEFAULT_HTTP_PORT.to_string())
        }
    };
    value
        .parse::<u16>()
        .map_err(|_| format!("HTTP port '{value}' must be between 0 and 65535"))
}

fn normalize_origin(value: &str) -> Result<String, String> {
    let url = Url::parse(value).map_err(|_| {
        format!("WORKSPACE_MCP_ALLOWED_ORIGINS contains an invalid origin: {value}")
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(format!(
            "WORKSPACE_MCP_ALLOWED_ORIGINS contains an invalid origin: {value}"
        ));
    }
    Ok(url.origin().ascii_serialization())
}

fn origin_of_url(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}

fn socket_address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("{name} is not valid Unicode: {error}")),
    }
}

async fn shutdown_signal() {
    let control_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("workspace-mcp-rs: failed to install Ctrl-C handler: {error}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                eprintln!("workspace-mcp-rs: failed to install SIGTERM handler: {error}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }

    #[cfg(not(unix))]
    control_c.await;
}
