use std::collections::HashSet;
use std::fmt::Write;

#[derive(Debug)]
pub struct ServiceMigration {
    pub service: &'static str,
    pub tools: &'static [&'static str],
    pub native_tools: &'static [&'static str],
}

const GMAIL: &[&str] = &[
    "search_gmail_messages",
    "get_gmail_message_content",
    "get_gmail_messages_content_batch",
    "send_gmail_message",
    "get_gmail_attachment_content",
    "get_gmail_thread_content",
    "modify_gmail_message_labels",
    "list_gmail_labels",
    "manage_gmail_label",
    "draft_gmail_message",
    "list_gmail_filters",
    "manage_gmail_filter",
    "get_gmail_threads_content_batch",
    "batch_modify_gmail_message_labels",
    "start_google_auth",
];
const GMAIL_NATIVE: &[&str] = &[
    "search_gmail_messages",
    "get_gmail_message_content",
    "get_gmail_messages_content_batch",
    "send_gmail_message",
    "get_gmail_attachment_content",
    "get_gmail_thread_content",
    "modify_gmail_message_labels",
    "list_gmail_labels",
    "manage_gmail_label",
    "draft_gmail_message",
    "list_gmail_filters",
    "manage_gmail_filter",
    "get_gmail_threads_content_batch",
    "batch_modify_gmail_message_labels",
    "start_google_auth",
];
const DRIVE: &[&str] = &[
    "search_drive_files",
    "get_drive_file_content",
    "get_drive_file_download_url",
    "create_drive_file",
    "create_drive_folder",
    "import_to_google_doc",
    "import_to_google_slides",
    "import_to_google_sheets",
    "get_drive_shareable_link",
    "list_drive_items",
    "copy_drive_file",
    "update_drive_file",
    "manage_drive_access",
    "set_drive_file_permissions",
    "get_drive_file_permissions",
    "check_drive_file_public_access",
];
const CALENDAR: &[&str] = &[
    "list_calendars",
    "get_events",
    "manage_event",
    "create_calendar",
    "query_freebusy",
    "manage_out_of_office",
    "manage_focus_time",
];
const DOCS: &[&str] = &[
    "get_doc_content",
    "create_doc",
    "modify_doc_text",
    "export_doc_to_pdf",
    "search_docs",
    "find_and_replace_doc",
    "list_docs_in_folder",
    "insert_doc_elements",
    "update_paragraph_style",
    "get_doc_as_markdown",
    "list_document_comments",
    "manage_document_comment",
    "insert_doc_image",
    "update_doc_headers_footers",
    "batch_update_doc",
    "inspect_doc_structure",
    "create_table_with_data",
    "debug_table_structure",
    "manage_doc_tab",
];
const SHEETS: &[&str] = &[
    "create_spreadsheet",
    "read_sheet_values",
    "modify_sheet_values",
    "list_spreadsheets",
    "get_spreadsheet_info",
    "format_sheet_range",
    "list_sheet_tables",
    "create_sheet",
    "append_table_rows",
    "resize_sheet_dimensions",
    "move_sheet_rows",
    "list_spreadsheet_comments",
    "manage_spreadsheet_comment",
    "manage_conditional_formatting",
];
const CHAT: &[&str] = &[
    "send_message",
    "get_messages",
    "search_messages",
    "create_reaction",
    "list_spaces",
    "download_chat_attachment",
];
const FORMS: &[&str] = &[
    "create_form",
    "get_form",
    "list_form_responses",
    "set_publish_settings",
    "get_form_response",
    "batch_update_form",
];
const SLIDES: &[&str] = &[
    "create_presentation",
    "get_presentation",
    "batch_update_presentation",
    "get_page",
    "get_page_thumbnail",
    "list_presentation_comments",
    "manage_presentation_comment",
];
const TASKS: &[&str] = &[
    "get_task",
    "list_tasks",
    "manage_task",
    "list_task_lists",
    "get_task_list",
    "manage_task_list",
];
const CONTACTS: &[&str] = &[
    "search_contacts",
    "get_contact",
    "list_contacts",
    "manage_contact",
    "list_contact_groups",
    "get_contact_group",
    "manage_contacts_batch",
    "manage_contact_group",
];
const SEARCH: &[&str] = &["search_custom", "get_search_engine_info"];
const APPSCRIPT: &[&str] = &[
    "list_script_projects",
    "get_script_project",
    "get_script_content",
    "create_script_project",
    "update_script_content",
    "run_script_function",
    "generate_trigger_code",
    "manage_deployment",
    "list_deployments",
    "delete_script_project",
    "list_versions",
    "create_version",
    "get_version",
    "list_script_processes",
    "get_script_metrics",
];

const SERVICES: &[ServiceMigration] = &[
    ServiceMigration {
        service: "gmail",
        tools: GMAIL,
        native_tools: GMAIL_NATIVE,
    },
    ServiceMigration {
        service: "drive",
        tools: DRIVE,
        native_tools: DRIVE,
    },
    ServiceMigration {
        service: "calendar",
        tools: CALENDAR,
        native_tools: CALENDAR,
    },
    ServiceMigration {
        service: "docs",
        tools: DOCS,
        native_tools: DOCS,
    },
    ServiceMigration {
        service: "sheets",
        tools: SHEETS,
        native_tools: SHEETS,
    },
    ServiceMigration {
        service: "chat",
        tools: CHAT,
        native_tools: CHAT,
    },
    ServiceMigration {
        service: "forms",
        tools: FORMS,
        native_tools: FORMS,
    },
    ServiceMigration {
        service: "slides",
        tools: SLIDES,
        native_tools: SLIDES,
    },
    ServiceMigration {
        service: "tasks",
        tools: TASKS,
        native_tools: TASKS,
    },
    ServiceMigration {
        service: "contacts",
        tools: CONTACTS,
        native_tools: CONTACTS,
    },
    ServiceMigration {
        service: "search",
        tools: SEARCH,
        native_tools: SEARCH,
    },
    ServiceMigration {
        service: "appscript",
        tools: APPSCRIPT,
        native_tools: APPSCRIPT,
    },
];

pub fn services() -> &'static [ServiceMigration] {
    SERVICES
}

pub fn all_native_tool_names() -> Vec<String> {
    services()
        .iter()
        .flat_map(|service| service.native_tools.iter().copied())
        .map(str::to_owned)
        .collect()
}

pub fn tool_service(tool_name: &str) -> Option<&'static str> {
    services()
        .iter()
        .find(|service| service.tools.contains(&tool_name))
        .map(|service| service.service)
}

pub fn native_tool_available_at_tier(tool_name: &str, tier: Option<&str>) -> bool {
    const CORE: &[&str] = &[
        "search_custom",
        "get_task",
        "list_tasks",
        "manage_task",
        "create_form",
        "get_form",
        "create_presentation",
        "get_presentation",
        "list_calendars",
        "get_events",
        "manage_event",
        "send_message",
        "get_messages",
        "search_messages",
        "create_reaction",
        "search_contacts",
        "get_contact",
        "list_contacts",
        "manage_contact",
        "search_gmail_messages",
        "get_gmail_message_content",
        "get_gmail_messages_content_batch",
        "send_gmail_message",
        "search_drive_files",
        "get_drive_file_content",
        "get_drive_file_download_url",
        "create_drive_file",
        "create_drive_folder",
        "import_to_google_doc",
        "import_to_google_slides",
        "import_to_google_sheets",
        "get_drive_shareable_link",
        "create_spreadsheet",
        "read_sheet_values",
        "modify_sheet_values",
        "get_doc_content",
        "create_doc",
        "modify_doc_text",
        "list_script_projects",
        "get_script_project",
        "get_script_content",
        "create_script_project",
        "update_script_content",
        "run_script_function",
        "generate_trigger_code",
    ];
    const EXTENDED: &[&str] = &[
        "list_form_responses",
        "batch_update_presentation",
        "get_page",
        "get_page_thumbnail",
        "create_calendar",
        "query_freebusy",
        "manage_out_of_office",
        "manage_focus_time",
        "list_spaces",
        "download_chat_attachment",
        "list_contact_groups",
        "get_contact_group",
        "get_gmail_attachment_content",
        "get_gmail_thread_content",
        "modify_gmail_message_labels",
        "list_gmail_labels",
        "manage_gmail_label",
        "draft_gmail_message",
        "list_gmail_filters",
        "manage_gmail_filter",
        "list_drive_items",
        "copy_drive_file",
        "update_drive_file",
        "manage_drive_access",
        "set_drive_file_permissions",
        "list_spreadsheets",
        "get_spreadsheet_info",
        "format_sheet_range",
        "list_sheet_tables",
        "export_doc_to_pdf",
        "search_docs",
        "find_and_replace_doc",
        "list_docs_in_folder",
        "insert_doc_elements",
        "update_paragraph_style",
        "get_doc_as_markdown",
        "list_document_comments",
        "manage_document_comment",
        "manage_deployment",
        "list_deployments",
        "delete_script_project",
        "list_versions",
        "create_version",
        "get_version",
        "list_script_processes",
        "get_script_metrics",
    ];

    match tier {
        None | Some("complete") => true,
        Some("extended") => CORE.contains(&tool_name) || EXTENDED.contains(&tool_name),
        Some("core") => CORE.contains(&tool_name),
        Some(_) => false,
    }
}

pub fn native_tool_is_read_only(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_custom"
            | "get_search_engine_info"
            | "get_task"
            | "list_tasks"
            | "list_task_lists"
            | "get_task_list"
            | "get_form"
            | "list_form_responses"
            | "get_form_response"
            | "get_presentation"
            | "get_page"
            | "get_page_thumbnail"
            | "list_presentation_comments"
            | "list_calendars"
            | "get_events"
            | "query_freebusy"
            | "list_spaces"
            | "get_messages"
            | "search_messages"
            | "download_chat_attachment"
            | "search_contacts"
            | "get_contact"
            | "list_contacts"
            | "list_contact_groups"
            | "get_contact_group"
            | "search_gmail_messages"
            | "get_gmail_message_content"
            | "get_gmail_messages_content_batch"
            | "get_gmail_attachment_content"
            | "get_gmail_thread_content"
            | "list_gmail_labels"
            | "get_gmail_threads_content_batch"
            | "search_drive_files"
            | "get_drive_file_content"
            | "get_drive_file_download_url"
            | "get_drive_shareable_link"
            | "list_drive_items"
            | "get_drive_file_permissions"
            | "check_drive_file_public_access"
            | "read_sheet_values"
            | "list_spreadsheets"
            | "get_spreadsheet_info"
            | "list_sheet_tables"
            | "list_spreadsheet_comments"
            | "get_doc_content"
            | "search_docs"
            | "list_docs_in_folder"
            | "get_doc_as_markdown"
            | "list_document_comments"
            | "inspect_doc_structure"
            | "debug_table_structure"
            | "list_script_projects"
            | "get_script_project"
            | "get_script_content"
            | "generate_trigger_code"
            | "list_deployments"
            | "list_versions"
            | "get_version"
            | "list_script_processes"
            | "get_script_metrics"
    )
}

pub fn validate_native_selection(requested: &[String]) -> Result<(), String> {
    if requested.is_empty() {
        return Ok(());
    }

    let known: HashSet<&str> = services()
        .iter()
        .flat_map(|service| service.tools.iter().copied())
        .collect();
    let native: HashSet<&str> = services()
        .iter()
        .flat_map(|service| service.native_tools.iter().copied())
        .collect();

    for tool in requested {
        if !known.contains(tool.as_str()) {
            return Err(format!(
                "WORKSPACE_MCP_RS_NATIVE_TOOLS contains unknown tool '{tool}'"
            ));
        }
        if !native.contains(tool.as_str()) {
            return Err(format!(
                "tool '{tool}' does not have a native Rust implementation"
            ));
        }
    }
    Ok(())
}

pub fn migration_status() -> String {
    let total: usize = services().iter().map(|service| service.tools.len()).sum();
    let native: usize = services()
        .iter()
        .map(|service| service.native_tools.len())
        .sum();
    let mut output = String::new();
    writeln!(output, "Google Workspace MCP native tool coverage").unwrap();
    writeln!(output, "native tools: {native}/{total}").unwrap();
    writeln!(output, "unavailable tools: {}/{}", total - native, total).unwrap();
    for service in services() {
        writeln!(
            output,
            "- {}: {}/{} native",
            service.service,
            service.native_tools.len(),
            service.tools.len()
        )
        .unwrap();
    }
    output
}
