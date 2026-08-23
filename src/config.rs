use std::collections::{HashMap, HashSet};
use std::env;

use crate::registry;

const NATIVE_TOOLS_ENV: &str = "WORKSPACE_MCP_RS_NATIVE_TOOLS";

#[derive(Debug, Clone)]
pub struct Config {
    pub requested_native_tools: Vec<String>,
}

pub struct NativeSelection {
    pub tools: HashSet<String>,
    pub restrict_tasks_manage_actions: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let requested_native_tools = match env_list(NATIVE_TOOLS_ENV) {
            Some(tools) => tools,
            None => registry::all_native_tool_names(),
        };
        Ok(Self {
            requested_native_tools,
        })
    }

    pub fn effective_native_selection(&self, args: &[String]) -> Result<NativeSelection, String> {
        let cli_services = cli_values(args, "--tools");
        let cli_permissions = cli_values(args, "--permissions");
        let cli_read_only = args.iter().any(|argument| argument == "--read-only");

        if cli_permissions.is_some() && cli_services.is_some() {
            return Err(String::from("--permissions and --tools cannot be combined"));
        }
        if cli_permissions.is_some() && cli_read_only {
            return Err(String::from(
                "--permissions and --read-only are mutually exclusive",
            ));
        }

        let services = if cli_permissions.is_some() {
            None
        } else {
            cli_services
                .clone()
                .or_else(|| env_list("WORKSPACE_MCP_TOOLS"))
        };
        let read_only = if cli_permissions.is_some() {
            false
        } else if cli_read_only {
            true
        } else {
            env_bool("WORKSPACE_MCP_READ_ONLY")?.unwrap_or(false)
        };
        let permission_values = if let Some(values) = cli_permissions {
            Some(values)
        } else if cli_read_only || cli_services.is_some() {
            None
        } else {
            env_list("WORKSPACE_MCP_PERMISSIONS")
        };

        if permission_values.is_some() && services.is_some() {
            return Err(String::from(
                "WORKSPACE_MCP_PERMISSIONS and WORKSPACE_MCP_TOOLS cannot be combined",
            ));
        }
        if permission_values.is_some() && read_only {
            return Err(String::from(
                "WORKSPACE_MCP_PERMISSIONS and WORKSPACE_MCP_READ_ONLY are mutually exclusive",
            ));
        }

        let permissions = permission_values
            .as_deref()
            .map(parse_permissions)
            .transpose()?;
        let disabled = cli_values(args, "--disabled-tools")
            .or_else(|| env_list("WORKSPACE_MCP_DISABLED_TOOLS"))
            .unwrap_or_default()
            .into_iter()
            .collect::<HashSet<_>>();
        let tier = cli_scalar(args, "--tool-tier")
            .or_else(|| env::var("WORKSPACE_MCP_TOOL_TIER").ok())
            .filter(|value| !value.trim().is_empty());
        if tier
            .as_deref()
            .is_some_and(|value| !matches!(value, "core" | "extended" | "complete"))
        {
            return Err(format!(
                "invalid tool tier '{}'; expected core, extended, or complete",
                tier.as_deref().unwrap_or_default()
            ));
        }

        let tools = self
            .requested_native_tools
            .iter()
            .filter(|tool| !disabled.contains(tool.as_str()))
            .filter(|tool| registry::native_tool_available_at_tier(tool, tier.as_deref()))
            .filter(|tool| {
                services.as_ref().is_none_or(|enabled| {
                    registry::tool_service(tool)
                        .is_some_and(|service| enabled.iter().any(|value| value == service))
                })
            })
            .filter(|tool| !read_only || registry::native_tool_is_read_only(tool))
            .filter(|tool| {
                permissions
                    .as_ref()
                    .is_none_or(|levels| permission_allows(tool, levels))
            })
            .cloned()
            .collect();

        Ok(NativeSelection {
            tools,
            restrict_tasks_manage_actions: permissions
                .as_ref()
                .and_then(|levels| levels.get("tasks"))
                .is_some_and(|level| level == "manage"),
        })
    }
}

#[derive(Clone, Copy)]
enum Access {
    Read(&'static str),
    Write(&'static str),
}

fn permission_allows(tool_name: &str, permissions: &HashMap<String, String>) -> bool {
    let Some(owner) = registry::tool_service(tool_name) else {
        return false;
    };
    if !permissions.contains_key(owner) {
        return false;
    }
    if owner == "gmail" {
        return gmail_permission_allows(tool_name, permissions);
    }
    if owner == "appscript" {
        return appscript_permission_allows(tool_name, permissions);
    }

    let requirement = match tool_name {
        "search_custom" | "get_search_engine_info" => Access::Read("search"),
        "get_task" | "list_tasks" | "list_task_lists" | "get_task_list" => Access::Read("tasks"),
        "manage_task" | "manage_task_list" => Access::Write("tasks"),
        "get_form" | "list_form_responses" | "get_form_response" => Access::Read("forms"),
        "create_form" | "set_publish_settings" | "batch_update_form" => Access::Write("forms"),
        "get_presentation" | "get_page" | "get_page_thumbnail" => Access::Read("slides"),
        "create_presentation" | "batch_update_presentation" => Access::Write("slides"),
        "list_presentation_comments" => Access::Read("drive"),
        "manage_presentation_comment" => Access::Write("drive"),
        "list_calendars" | "get_events" | "query_freebusy" => Access::Read("calendar"),
        "manage_event" | "manage_out_of_office" | "manage_focus_time" | "create_calendar" => {
            Access::Write("calendar")
        }
        "search_contacts"
        | "get_contact"
        | "list_contacts"
        | "list_contact_groups"
        | "get_contact_group" => Access::Read("contacts"),
        "manage_contact" | "manage_contacts_batch" | "manage_contact_group" => {
            Access::Write("contacts")
        }
        "list_spaces" | "download_chat_attachment" => Access::Read("chat"),
        "send_message" | "create_reaction" => Access::Write("chat"),
        "get_messages" | "search_messages" => {
            return access_granted(Access::Read("chat"), permissions)
                && access_granted(Access::Read("contacts"), permissions);
        }
        "search_drive_files"
        | "get_drive_file_content"
        | "get_drive_file_download_url"
        | "get_drive_shareable_link"
        | "list_drive_items"
        | "get_drive_file_permissions"
        | "check_drive_file_public_access" => Access::Read("drive"),
        "create_drive_file"
        | "create_drive_folder"
        | "import_to_google_doc"
        | "import_to_google_slides"
        | "import_to_google_sheets"
        | "copy_drive_file"
        | "update_drive_file"
        | "manage_drive_access"
        | "set_drive_file_permissions" => Access::Write("drive"),
        "read_sheet_values" | "get_spreadsheet_info" | "list_sheet_tables" => {
            Access::Read("sheets")
        }
        "list_spreadsheets" | "list_spreadsheet_comments" => Access::Read("sheets"),
        "manage_spreadsheet_comment" => Access::Write("drive"),
        "create_spreadsheet"
        | "modify_sheet_values"
        | "format_sheet_range"
        | "create_sheet"
        | "append_table_rows"
        | "resize_sheet_dimensions"
        | "move_sheet_rows"
        | "manage_conditional_formatting" => Access::Write("sheets"),
        "get_doc_content"
        | "search_docs"
        | "list_docs_in_folder"
        | "get_doc_as_markdown"
        | "list_document_comments"
        | "inspect_doc_structure"
        | "debug_table_structure" => Access::Read("docs"),
        "manage_document_comment" => Access::Write("drive"),
        "create_doc"
        | "modify_doc_text"
        | "export_doc_to_pdf"
        | "find_and_replace_doc"
        | "insert_doc_elements"
        | "update_paragraph_style"
        | "insert_doc_image"
        | "update_doc_headers_footers"
        | "batch_update_doc"
        | "create_table_with_data"
        | "manage_doc_tab" => Access::Write("docs"),
        _ => return false,
    };
    access_granted(requirement, permissions)
}

fn appscript_permission_allows(tool_name: &str, permissions: &HashMap<String, String>) -> bool {
    let Some(level) = permissions.get("appscript").map(String::as_str) else {
        return false;
    };
    match tool_name {
        "list_script_projects"
        | "get_script_project"
        | "get_script_content"
        | "generate_trigger_code"
        | "list_deployments"
        | "list_versions"
        | "get_version"
        | "list_script_processes"
        | "get_script_metrics" => matches!(level, "readonly" | "full"),
        "create_script_project"
        | "update_script_content"
        | "manage_deployment"
        | "create_version" => level == "full",
        "delete_script_project" => {
            level == "full" && access_granted(Access::Write("drive"), permissions)
        }
        "run_script_function" => false,
        _ => false,
    }
}

fn gmail_permission_allows(tool_name: &str, permissions: &HashMap<String, String>) -> bool {
    let Some(level) = permissions.get("gmail").map(String::as_str) else {
        return false;
    };
    let granted_rank = match level {
        "readonly" => 0,
        "organize" => 1,
        "drafts" => 2,
        "send" => 3,
        "full" => 4,
        _ => return false,
    };
    let required_rank = match tool_name {
        "search_gmail_messages"
        | "get_gmail_message_content"
        | "get_gmail_messages_content_batch"
        | "get_gmail_attachment_content"
        | "get_gmail_thread_content"
        | "list_gmail_labels"
        | "get_gmail_threads_content_batch" => 0,
        "modify_gmail_message_labels"
        | "manage_gmail_label"
        | "batch_modify_gmail_message_labels" => 1,
        "draft_gmail_message" => 2,
        "send_gmail_message" => 3,
        "list_gmail_filters" | "manage_gmail_filter" => 4,
        "start_google_auth" => return true,
        _ => return false,
    };
    granted_rank >= required_rank
}

fn access_granted(requirement: Access, permissions: &HashMap<String, String>) -> bool {
    let (service, write) = match requirement {
        Access::Read(service) => (service, false),
        Access::Write(service) => (service, true),
    };
    let Some(level) = permissions.get(service).map(String::as_str) else {
        return false;
    };

    matches!(
        (service, write, level),
        ("tasks", false, "readonly" | "manage" | "full")
            | ("tasks", true, "manage" | "full")
            | (_, false, "readonly" | "full")
            | (_, true, "full")
    )
}

fn parse_permissions(values: &[String]) -> Result<HashMap<String, String>, String> {
    let mut result = HashMap::new();
    for entry in values {
        let Some((service, level)) = entry.split_once(':') else {
            return Err(format!(
                "invalid permission format '{entry}'; expected service:level"
            ));
        };
        if result.contains_key(service) {
            return Err(format!("duplicate service in permissions: '{service}'"));
        }
        let valid = match service {
            "tasks" => matches!(level, "readonly" | "manage" | "full"),
            "gmail" => matches!(level, "readonly" | "organize" | "drafts" | "send" | "full"),
            "drive" | "calendar" | "docs" | "sheets" | "chat" | "forms" | "slides" | "contacts"
            | "search" | "appscript" => matches!(level, "readonly" | "full"),
            _ => false,
        };
        if !valid {
            return Err(format!("unknown permission service or level: '{entry}'"));
        }
        result.insert(service.to_owned(), level.to_owned());
    }
    Ok(result)
}

fn env_bool(name: &str) -> Result<Option<bool>, String> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(Some(true)),
            "false" | "0" | "no" => Ok(Some(false)),
            _ => Err(format!(
                "invalid {name} '{value}'; expected true/1/yes or false/0/no"
            )),
        },
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("{name} is not valid Unicode: {error}")),
    }
}

fn env_list(name: &str) -> Option<Vec<String>> {
    env::var(name).ok().map(|value| {
        value
            .split(|character: char| character == ',' || character.is_whitespace())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

fn cli_scalar(args: &[String], flag: &str) -> Option<String> {
    args.iter().enumerate().find_map(|(index, argument)| {
        if argument == flag {
            args.get(index + 1).cloned()
        } else {
            argument
                .strip_prefix(&format!("{flag}="))
                .map(str::to_owned)
        }
    })
}

fn cli_values(args: &[String], flag: &str) -> Option<Vec<String>> {
    let index = args
        .iter()
        .position(|argument| argument == flag || argument.starts_with(&format!("{flag}=")))?;
    if let Some(value) = args[index].strip_prefix(&format!("{flag}=")) {
        return Some(
            value
                .split(',')
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
        );
    }
    Some(
        args[index + 1..]
            .iter()
            .take_while(|argument| !argument.starts_with('-'))
            .cloned()
            .collect(),
    )
}
