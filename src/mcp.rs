use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use jsonschema::Validator;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::native;

const LEGACY_CATALOG: &str = include_str!("../contracts/tools-legacy.json");
const OAUTH21_CATALOG: &str = include_str!("../contracts/tools-oauth21.json");
const GATEWAY_CATALOG: &str = include_str!("../contracts/tools-gateway.json");

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// The newest MCP protocol version implemented by this engine.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = [MCP_PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

/// Selects which embedded tool contract the MCP engine exposes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpMode {
    Legacy,
    Oauth21,
    Gateway,
}

impl McpMode {
    fn catalog(self) -> &'static str {
        match self {
            Self::Legacy => LEGACY_CATALOG,
            Self::Oauth21 => OAUTH21_CATALOG,
            Self::Gateway => GATEWAY_CATALOG,
        }
    }
}

/// Lifecycle state owned by one stdio connection or HTTP session.
#[derive(Debug, Default)]
pub struct McpSessionState {
    initialize_received: AtomicBool,
    initialized: AtomicBool,
}

impl McpSessionState {
    /// Construct a lifecycle-ready ephemeral state for stateless HTTP tool and
    /// ping requests. This carries no identity or transport session data.
    pub fn stateless_ready() -> Self {
        Self {
            initialize_received: AtomicBool::new(true),
            initialized: AtomicBool::new(true),
        }
    }

    /// Construct the ephemeral state used for an initialized notification in
    /// stateless HTTP mode.
    pub fn stateless_initialize_pending() -> Self {
        Self {
            initialize_received: AtomicBool::new(true),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn initialize_received(&self) -> bool {
        self.initialize_received.load(Ordering::Acquire)
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }
}

/// An MCP tool projected from an embedded contract.
#[derive(Clone, Debug, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema")]
    pub output_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// Text content in an MCP call result.
#[derive(Clone, Debug, Serialize)]
pub struct McpTextContent {
    #[serde(rename = "type")]
    pub content_type: &'static str,
    pub text: String,
}

/// Transport-neutral MCP `CallToolResult`.
#[derive(Clone, Debug, Serialize)]
pub struct McpCallToolResult {
    pub content: Vec<McpTextContent>,
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

impl McpCallToolResult {
    fn success(text: String) -> Self {
        Self {
            content: vec![McpTextContent {
                content_type: "text",
                text: text.clone(),
            }],
            structured_content: Some(json!({ "result": text })),
            is_error: false,
        }
    }

    fn tool_error(error: String) -> Self {
        Self {
            content: vec![McpTextContent {
                content_type: "text",
                text: error,
            }],
            structured_content: None,
            is_error: true,
        }
    }

    fn into_value(self) -> Value {
        let mut result = Map::new();
        result.insert("content".to_owned(), json!(self.content));
        if let Some(structured_content) = self.structured_content {
            result.insert("structuredContent".to_owned(), structured_content);
        }
        result.insert("isError".to_owned(), Value::Bool(self.is_error));
        Value::Object(result)
    }
}

/// Failure to parse an embedded catalog or compile one of its schemas.
#[derive(Debug)]
pub struct McpEngineError {
    message: String,
}

impl McpEngineError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for McpEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for McpEngineError {}

/// An invalid `tools/call` request. Native tool failures are returned as
/// `McpCallToolResult` with `is_error` set instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpCallError {
    pub message: String,
}

impl fmt::Display for McpCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for McpCallError {}

struct ToolRuntime {
    validator: Validator,
    aliases: HashMap<String, String>,
}

/// Native MCP JSON-RPC engine. It contains no framing or HTTP assumptions and
/// can be shared behind an `Arc` by stdio and Axum adapters.
pub struct McpEngine {
    tools: Vec<McpTool>,
    runtimes: HashMap<String, ToolRuntime>,
    restrict_tasks_manage_actions: bool,
}

impl McpEngine {
    /// Builds an engine from one embedded contract and retains only names in
    /// `selected_tools`. An empty set intentionally exposes no tools.
    pub fn new(
        mode: McpMode,
        selected_tools: &HashSet<String>,
        restrict_tasks_manage_actions: bool,
    ) -> Result<Self, McpEngineError> {
        let catalog: ContractCatalog = serde_json::from_str(mode.catalog()).map_err(|error| {
            McpEngineError::new(format!("failed to parse embedded MCP catalog: {error}"))
        })?;
        let mut tools = Vec::new();
        let mut runtimes = HashMap::new();

        for contract_tool in catalog.tools {
            if !selected_tools.contains(&contract_tool.name) {
                continue;
            }
            if runtimes.contains_key(&contract_tool.name) {
                return Err(McpEngineError::new(format!(
                    "duplicate tool '{}' in embedded MCP catalog",
                    contract_tool.name
                )));
            }

            let aliases = argument_aliases(&contract_tool.name, &contract_tool.parameters)?;
            let validator =
                jsonschema::validator_for(&contract_tool.parameters).map_err(|error| {
                    McpEngineError::new(format!(
                        "invalid parameter schema for tool '{}': {error}",
                        contract_tool.name
                    ))
                })?;
            let name = contract_tool.name.clone();
            let tool = McpTool {
                name: contract_tool.name,
                title: contract_tool.title,
                description: contract_tool.description,
                input_schema: contract_tool.parameters,
                output_schema: contract_tool.output_schema,
                annotations: contract_tool.annotations,
                icons: contract_tool.icons,
                meta: contract_tool.meta,
            };
            tools.push(tool);
            runtimes.insert(name, ToolRuntime { validator, aliases });
        }

        Ok(Self {
            tools,
            runtimes,
            restrict_tasks_manage_actions,
        })
    }

    /// Validates and dispatches a native tool independently of JSON-RPC.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<McpCallToolResult, McpCallError> {
        let runtime = self.runtimes.get(name).ok_or_else(|| McpCallError {
            message: format!("unknown or unavailable tool '{name}'"),
        })?;
        let normalized = normalize_arguments(arguments, &runtime.aliases)?;
        let instance = Value::Object(normalized.clone());
        if let Err(error) = runtime.validator.validate(&instance) {
            return Err(McpCallError {
                message: format!("invalid arguments for tool '{name}': {error}"),
            });
        }

        let result = native::dispatch(name, &normalized, self.restrict_tasks_manage_actions).await;
        Ok(match result {
            Ok(text) => McpCallToolResult::success(text),
            Err(error) => McpCallToolResult::tool_error(error),
        })
    }

    /// Parses and handles one UTF-8 JSON-RPC message. This is convenient for
    /// newline-framed stdio; framing and output writes remain the caller's job.
    pub async fn handle_bytes(&self, session: &McpSessionState, message: &[u8]) -> Option<Value> {
        match serde_json::from_slice(message) {
            Ok(request) => self.handle_value(session, request).await,
            Err(error) => Some(error_response(
                Value::Null,
                RpcError::with_data(PARSE_ERROR, "Parse error", Value::String(error.to_string())),
            )),
        }
    }

    /// Handles one already-decoded JSON-RPC message. Axum handlers can pass a
    /// `Json<Value>` body here and serialize the optional response directly.
    pub async fn handle_value(&self, session: &McpSessionState, request: Value) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(error_response(
                Value::Null,
                RpcError::new(INVALID_REQUEST, "Invalid Request"),
            ));
        };

        let id = valid_id(object.get("id"));
        let is_notification = !object.contains_key("id");
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || object.get("method").and_then(Value::as_str).is_none()
            || (object.contains_key("id") && id.is_none())
        {
            return Some(error_response(
                id.unwrap_or(Value::Null),
                RpcError::new(INVALID_REQUEST, "Invalid Request"),
            ));
        }

        let method = match object.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                return Some(error_response(
                    Value::Null,
                    RpcError::new(INVALID_REQUEST, "Invalid Request"),
                ));
            }
        };
        let outcome = self
            .route(session, method, object.get("params"), is_notification)
            .await;
        if is_notification {
            return None;
        }

        let response_id = id.unwrap_or(Value::Null);
        Some(match outcome {
            Ok(result) => success_response(response_id, result),
            Err(error) => error_response(response_id, error),
        })
    }

    async fn route(
        &self,
        session: &McpSessionState,
        method: &str,
        params: Option<&Value>,
        is_notification: bool,
    ) -> Result<Value, RpcError> {
        match method {
            "initialize" if !is_notification => initialize(session, params),
            "initialize" => Err(RpcError::new(INVALID_REQUEST, "Invalid Request")),
            "notifications/initialized" if is_notification => initialized(session, params),
            "notifications/initialized" => Err(RpcError::new(INVALID_REQUEST, "Invalid Request")),
            "ping" => {
                optional_object_params(params)?;
                Ok(json!({}))
            }
            "tools/list" => {
                require_initialized(session)?;
                list_params(params)?;
                Ok(json!({ "tools": self.tools }))
            }
            "tools/call" => {
                require_initialized(session)?;
                let (name, arguments) = call_params(params)?;
                self.call_tool(name, arguments)
                    .await
                    .map(McpCallToolResult::into_value)
                    .map_err(|error| RpcError::new(INVALID_PARAMS, error.message))
            }
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "Method not found")),
        }
    }
}

#[derive(Deserialize)]
struct ContractCatalog {
    tools: Vec<ContractTool>,
}

#[derive(Deserialize)]
struct ContractTool {
    name: String,
    title: Option<String>,
    description: Option<String>,
    parameters: Value,
    output_schema: Value,
    annotations: Option<Value>,
    icons: Option<Value>,
    meta: Option<Value>,
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

fn argument_aliases(
    tool_name: &str,
    schema: &Value,
) -> Result<HashMap<String, String>, McpEngineError> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            McpEngineError::new(format!(
                "parameter schema for tool '{tool_name}' has no object properties"
            ))
        })?;
    let mut aliases = HashMap::new();
    for canonical in properties.keys() {
        let alias = snake_to_lower_camel(canonical);
        if alias == *canonical {
            continue;
        }
        if properties.contains_key(&alias) {
            return Err(McpEngineError::new(format!(
                "parameter schema for tool '{tool_name}' contains ambiguous keys '{canonical}' and '{alias}'"
            )));
        }
        if let Some(existing) = aliases.insert(alias.clone(), canonical.clone()) {
            return Err(McpEngineError::new(format!(
                "parameter schema for tool '{tool_name}' maps both '{existing}' and '{canonical}' to alias '{alias}'"
            )));
        }
    }
    Ok(aliases)
}

fn snake_to_lower_camel(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut uppercase = false;
    for character in name.chars() {
        if character == '_' {
            uppercase = true;
        } else if uppercase {
            result.extend(character.to_uppercase());
            uppercase = false;
        } else {
            result.push(character);
        }
    }
    result
}

fn normalize_arguments(
    arguments: Map<String, Value>,
    aliases: &HashMap<String, String>,
) -> Result<Map<String, Value>, McpCallError> {
    for (alias, canonical) in aliases {
        if arguments.contains_key(alias) && arguments.contains_key(canonical) {
            return Err(McpCallError {
                message: format!(
                    "arguments '{canonical}' and its camelCase alias '{alias}' cannot both be supplied"
                ),
            });
        }
    }

    let mut normalized = Map::new();
    for (key, value) in arguments {
        let canonical = aliases.get(&key).cloned().unwrap_or(key);
        if normalized.insert(canonical.clone(), value).is_some() {
            return Err(McpCallError {
                message: format!("argument '{canonical}' was supplied more than once"),
            });
        }
    }
    Ok(normalized)
}

fn initialize(session: &McpSessionState, params: Option<&Value>) -> Result<Value, RpcError> {
    if session.initialize_received() {
        return Err(RpcError::new(INVALID_REQUEST, "Invalid Request"));
    }
    let params = required_object_params(params)?;
    let requested_version = required_string(params, "protocolVersion")?;
    let capabilities = params.get("capabilities").and_then(Value::as_object);
    let client_info = params.get("clientInfo").and_then(Value::as_object);
    if capabilities.is_none() || client_info.is_none() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "initialize requires object capabilities and clientInfo",
        ));
    }
    let client_info = match client_info {
        Some(client_info) => client_info,
        None => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "initialize requires clientInfo",
            ));
        }
    };
    required_string(client_info, "name")?;
    required_string(client_info, "version")?;

    if session
        .initialize_received
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(RpcError::new(INVALID_REQUEST, "Invalid Request"));
    }
    let protocol_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested_version) {
        requested_version
    } else {
        MCP_PROTOCOL_VERSION
    };
    Ok(json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION")
        }
    }))
}

fn initialized(session: &McpSessionState, params: Option<&Value>) -> Result<Value, RpcError> {
    optional_object_params(params)?;
    if !session.initialize_received() {
        return Err(RpcError::new(INVALID_REQUEST, "Invalid Request"));
    }
    session.initialized.store(true, Ordering::Release);
    Ok(json!({}))
}

fn require_initialized(session: &McpSessionState) -> Result<(), RpcError> {
    if session.is_initialized() {
        Ok(())
    } else {
        Err(RpcError::new(INVALID_REQUEST, "Server not initialized"))
    }
}

fn optional_object_params(params: Option<&Value>) -> Result<(), RpcError> {
    match params {
        None | Some(Value::Object(_)) => Ok(()),
        Some(_) => Err(RpcError::new(INVALID_PARAMS, "params must be an object")),
    }
}

fn required_object_params(params: Option<&Value>) -> Result<&Map<String, Value>, RpcError> {
    params
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "params must be an object"))
}

fn list_params(params: Option<&Value>) -> Result<(), RpcError> {
    optional_object_params(params)?;
    let Some(params) = params.and_then(Value::as_object) else {
        return Ok(());
    };
    match params.get("cursor") {
        None | Some(Value::Null) => Ok(()),
        Some(_) => Err(RpcError::new(
            INVALID_PARAMS,
            "pagination cursors are not supported",
        )),
    }
}

fn call_params(params: Option<&Value>) -> Result<(&str, Map<String, Value>), RpcError> {
    let params = required_object_params(params)?;
    let name = required_string(params, "name")?;
    let arguments = match params.get("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "tools/call arguments must be an object",
            ));
        }
    };
    Ok((name, arguments))
}

fn required_string<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, RpcError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("{name} must be a non-empty string")))
}

fn valid_id(id: Option<&Value>) -> Option<Value> {
    match id {
        Some(value @ (Value::Null | Value::String(_) | Value::Number(_))) => Some(value.clone()),
        Some(_) | None => None,
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(id: Value, error: RpcError) -> Value {
    let mut body = Map::new();
    body.insert("code".to_owned(), Value::Number(error.code.into()));
    body.insert("message".to_owned(), Value::String(error.message));
    if let Some(data) = error.data {
        body.insert("data".to_owned(), data);
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": body
    })
}
