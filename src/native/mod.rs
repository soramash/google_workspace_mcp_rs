pub mod appscript;
pub mod calendar;
pub mod chat;
pub mod common;
pub mod contacts;
pub mod docs;
pub mod drive;
pub mod forms;
pub mod gmail;
pub mod search;
pub mod sheets;
pub mod slides;
pub mod tasks;

use serde_json::{Map, Value};

use crate::{auth, oauth, registry};

pub async fn dispatch(
    tool_name: &str,
    arguments: &Map<String, Value>,
    restrict_tasks_manage_actions: bool,
) -> Result<String, String> {
    if tool_name == "start_google_auth" {
        let service_name = arguments
            .get("service_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| String::from("service_name is required"))?;
        let principal = auth::request_principal_email();
        let requested = arguments
            .get("user_google_email")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        let email = match (principal, requested) {
            (Some(principal), Some(requested))
                if !principal.eq_ignore_ascii_case(requested.trim()) =>
            {
                return Err(String::from(
                    "The requested Google account does not match the authenticated principal",
                ));
            }
            (Some(principal), _) => principal,
            (None, Some(requested)) => requested.trim().to_owned(),
            (None, None) => {
                return Err(String::from(
                    "No authoritative Google principal is available for authorization",
                ));
            }
        };
        return oauth::start_google_auth(service_name, &email);
    }

    match registry::tool_service(tool_name) {
        Some("appscript") => appscript::dispatch(tool_name, arguments).await,
        Some("search") => match tool_name {
            "search_custom" => search::search_custom(arguments).await,
            "get_search_engine_info" => search::get_search_engine_info(arguments).await,
            _ => Err(not_implemented(tool_name)),
        },
        Some("calendar") => calendar::dispatch(tool_name, arguments).await,
        Some("chat") => chat::dispatch(tool_name, arguments).await,
        Some("contacts") => contacts::dispatch(tool_name, arguments).await,
        Some("docs") => docs::dispatch(tool_name, arguments).await,
        Some("drive") => drive::dispatch(tool_name, arguments).await,
        Some("forms") => forms::dispatch(tool_name, arguments).await,
        Some("gmail") if tool_name != "start_google_auth" => {
            gmail::dispatch(tool_name, arguments).await
        }
        Some("sheets") => sheets::dispatch(tool_name, arguments).await,
        Some("slides") => slides::dispatch(tool_name, arguments).await,
        Some("tasks") => tasks::dispatch(tool_name, arguments, restrict_tasks_manage_actions).await,
        _ => Err(not_implemented(tool_name)),
    }
}

fn not_implemented(tool_name: &str) -> String {
    format!("native Rust tool '{tool_name}' is not implemented")
}
