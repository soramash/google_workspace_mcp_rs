//! Native Google Docs tools.
//!
//! This module deliberately builds every Docs request from a typed allowlist. User
//! supplied JSON is never forwarded to Google as an arbitrary request object.

use std::env;
use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Map, Value, json};

use super::common::comments::{self, CommentAction};
use super::common::safe_fetch::SafeFetcher;
use super::common::url_safety::{sanitize_link_url, validate_link_url};
use crate::auth::GoogleSession;

const DOCS_API: &str = "https://docs.googleapis.com/v1/documents";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3/files";
const DRIVE_UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3/files";
const DOCS_READ: &str = "https://www.googleapis.com/auth/documents.readonly";
const DOCS_WRITE: &str = "https://www.googleapis.com/auth/documents";
const DRIVE_READ: &str = "https://www.googleapis.com/auth/drive.readonly";
const DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";
const GOOGLE_DOC_MIME: &str = "application/vnd.google-apps.document";
const MAX_TEXT_CHARS: usize = 1_000_000;
const MAX_CONTENT_BYTES_DEFAULT: usize = 25 * 1024 * 1024;
const MAX_IMAGE_BYTES_DEFAULT: usize = 10 * 1024 * 1024;
const MAX_BATCH_INPUT_BYTES: usize = 10 * 1024 * 1024;
const MAX_GENERATED_REQUESTS: usize = 10_000;
const MAX_EXPANDING_INDICES: usize = 2_000;
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Clone, Copy)]
enum Kind {
    String,
    Integer,
    NullableNumber,
    Boolean,
    NullableString,
    NullableInteger,
    NullableBoolean,
    NullableStringArray,
    StringMatrix,
    Operations,
}

pub async fn dispatch(tool_name: &str, arguments: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(arguments, tool_schema(tool_name)?)?;
    match tool_name {
        "search_docs" => search_docs(&a).await,
        "get_doc_content" => get_doc_content(&a).await,
        "list_docs_in_folder" => list_docs_in_folder(&a).await,
        "create_doc" => create_doc(&a).await,
        "modify_doc_text" => modify_doc_text(&a).await,
        "find_and_replace_doc" => find_and_replace_doc(&a).await,
        "insert_doc_elements" => insert_doc_elements(&a).await,
        "insert_doc_image" => insert_doc_image(&a).await,
        "update_doc_headers_footers" => update_doc_headers_footers(&a).await,
        "batch_update_doc" => batch_update_doc(&a).await,
        "inspect_doc_structure" => inspect_doc_structure(&a).await,
        "create_table_with_data" => create_table_with_data(&a).await,
        "debug_table_structure" => debug_table_structure(&a).await,
        "export_doc_to_pdf" => export_doc_to_pdf(&a).await,
        "update_paragraph_style" => update_paragraph_style(&a).await,
        "get_doc_as_markdown" => get_doc_as_markdown(&a).await,
        "manage_doc_tab" => manage_doc_tab(&a).await,
        "list_document_comments" => list_document_comments(&a).await,
        "manage_document_comment" => manage_document_comment(&a).await,
        _ => Err(format!(
            "native Rust Docs tool '{tool_name}' is not implemented"
        )),
    }
}

fn tool_schema(name: &str) -> Result<&'static [(&'static str, Kind)], String> {
    const EMAIL: (&str, Kind) = ("user_google_email", Kind::NullableString);
    const SEARCH: &[(&str, Kind)] = &[EMAIL, ("query", Kind::String), ("page_size", Kind::Integer)];
    const GET: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("suggestions_view_mode", Kind::String),
    ];
    const LIST: &[(&str, Kind)] = &[
        EMAIL,
        ("folder_id", Kind::String),
        ("page_size", Kind::Integer),
    ];
    const CREATE: &[(&str, Kind)] = &[EMAIL, ("title", Kind::String), ("content", Kind::String)];
    const MODIFY: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("start_index", Kind::Integer),
        ("end_index", Kind::NullableInteger),
        ("text", Kind::NullableString),
        ("tab_id", Kind::NullableString),
        ("segment_id", Kind::NullableString),
        ("end_of_segment", Kind::Boolean),
        ("bold", Kind::NullableBoolean),
        ("italic", Kind::NullableBoolean),
        ("underline", Kind::NullableBoolean),
        ("strikethrough", Kind::NullableBoolean),
        ("font_size", Kind::NullableNumber),
        ("font_family", Kind::NullableString),
        ("font_weight", Kind::NullableInteger),
        ("text_color", Kind::NullableString),
        ("background_color", Kind::NullableString),
        ("link_url", Kind::NullableString),
        ("clear_link", Kind::NullableBoolean),
        ("baseline_offset", Kind::NullableString),
        ("small_caps", Kind::NullableBoolean),
    ];
    const FIND: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("find_text", Kind::String),
        ("replace_text", Kind::String),
        ("match_case", Kind::Boolean),
        ("tab_id", Kind::NullableString),
    ];
    const ELEMENTS: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("element_type", Kind::String),
        ("index", Kind::Integer),
        ("rows", Kind::NullableInteger),
        ("columns", Kind::NullableInteger),
        ("list_type", Kind::NullableString),
        ("text", Kind::NullableString),
    ];
    const IMAGE: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("image_source", Kind::String),
        ("index", Kind::Integer),
        ("width", Kind::Integer),
        ("height", Kind::Integer),
    ];
    const HEADER: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("section_type", Kind::String),
        ("content", Kind::String),
        ("header_footer_type", Kind::String),
    ];
    const BATCH: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("operations", Kind::Operations),
    ];
    const INSPECT: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("detailed", Kind::Boolean),
        ("tab_id", Kind::NullableString),
    ];
    const TABLE: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("table_data", Kind::StringMatrix),
        ("index", Kind::Integer),
        ("bold_headers", Kind::Boolean),
        ("tab_id", Kind::NullableString),
        ("header_rows", Kind::Integer),
    ];
    const DEBUG_TABLE: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("table_index", Kind::Integer),
    ];
    const PDF: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("pdf_filename", Kind::NullableString),
        ("folder_id", Kind::NullableString),
    ];
    const PARA: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("start_index", Kind::Integer),
        ("end_index", Kind::Integer),
        ("heading_level", Kind::NullableInteger),
        ("alignment", Kind::NullableString),
        ("line_spacing", Kind::NullableNumber),
        ("indent_first_line", Kind::NullableNumber),
        ("indent_start", Kind::NullableNumber),
        ("indent_end", Kind::NullableNumber),
        ("space_above", Kind::NullableNumber),
        ("space_below", Kind::NullableNumber),
        ("named_style_type", Kind::NullableString),
        ("tab_id", Kind::NullableString),
        ("segment_id", Kind::NullableString),
        ("direction", Kind::NullableString),
        ("keep_lines_together", Kind::NullableBoolean),
        ("keep_with_next", Kind::NullableBoolean),
        ("avoid_widow_and_orphan", Kind::NullableBoolean),
        ("page_break_before", Kind::NullableBoolean),
        ("spacing_mode", Kind::NullableString),
        ("shading_color", Kind::NullableString),
        ("border_edges", Kind::NullableStringArray),
        ("border_color", Kind::NullableString),
        ("border_width", Kind::NullableNumber),
        ("border_padding", Kind::NullableNumber),
        ("border_dash", Kind::NullableString),
        ("list_type", Kind::NullableString),
        ("list_nesting_level", Kind::NullableInteger),
        ("bullet_preset", Kind::NullableString),
    ];
    const MARKDOWN: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("include_comments", Kind::Boolean),
        ("comment_mode", Kind::String),
        ("include_resolved", Kind::Boolean),
        ("suggestions_view_mode", Kind::String),
    ];
    const TAB: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("action", Kind::String),
        ("tab_id", Kind::NullableString),
        ("title", Kind::NullableString),
        ("index", Kind::NullableInteger),
        ("parent_tab_id", Kind::NullableString),
        ("markdown_text", Kind::NullableString),
        ("replace_existing", Kind::Boolean),
    ];
    const COMMENTS: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("max_comments", Kind::NullableInteger),
    ];
    const MANAGE_COMMENT: &[(&str, Kind)] = &[
        EMAIL,
        ("document_id", Kind::String),
        ("action", Kind::String),
        ("comment_content", Kind::NullableString),
        ("comment_id", Kind::NullableString),
    ];
    Ok(match name {
        "search_docs" => SEARCH,
        "get_doc_content" => GET,
        "list_docs_in_folder" => LIST,
        "create_doc" => CREATE,
        "modify_doc_text" => MODIFY,
        "find_and_replace_doc" => FIND,
        "insert_doc_elements" => ELEMENTS,
        "insert_doc_image" => IMAGE,
        "update_doc_headers_footers" => HEADER,
        "batch_update_doc" => BATCH,
        "inspect_doc_structure" => INSPECT,
        "create_table_with_data" => TABLE,
        "debug_table_structure" => DEBUG_TABLE,
        "export_doc_to_pdf" => PDF,
        "update_paragraph_style" => PARA,
        "get_doc_as_markdown" => MARKDOWN,
        "manage_doc_tab" => TAB,
        "list_document_comments" => COMMENTS,
        "manage_document_comment" => MANAGE_COMMENT,
        _ => return Err(format!("Unknown Docs tool: {name}")),
    })
}

fn normalize(
    raw: &Map<String, Value>,
    schema: &[(&str, Kind)],
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (key, value) in raw {
        let Some((canonical, kind)) = schema
            .iter()
            .find(|(name, _)| key == *name || key == &snake_to_camel(name))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        if out.contains_key(*canonical) {
            return Err(format!("Unexpected argument: {key}"));
        }
        validate_kind(value, *kind, canonical)?;
        out.insert((*canonical).to_owned(), value.clone());
    }
    Ok(out)
}

fn validate_kind(value: &Value, kind: Kind, name: &str) -> Result<(), String> {
    let nullable = matches!(
        kind,
        Kind::NullableString
            | Kind::NullableInteger
            | Kind::NullableNumber
            | Kind::NullableBoolean
            | Kind::NullableStringArray
    );
    if value.is_null() {
        return if nullable {
            Ok(())
        } else {
            Err(format!("{name} must not be null"))
        };
    }
    let parsed = if matches!(kind, Kind::Operations) {
        container(value, name)?
    } else {
        value.clone()
    };
    let valid = match kind {
        Kind::String | Kind::NullableString => parsed.is_string(),
        Kind::Integer | Kind::NullableInteger => parsed.as_i64().is_some(),
        Kind::NullableNumber => parsed.as_f64().is_some(),
        Kind::Boolean | Kind::NullableBoolean => parsed.is_boolean(),
        Kind::Operations => parsed
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_object)),
        Kind::NullableStringArray => parsed
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string)),
        Kind::StringMatrix => parsed.as_array().is_some_and(|rows| {
            rows.iter().all(|r| {
                r.as_array()
                    .is_some_and(|cells| cells.iter().all(Value::is_string))
            })
        }),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid type for argument: {name}"))
    }
}

fn container(value: &Value, name: &str) -> Result<Value, String> {
    if let Some(raw) = value.as_str() {
        serde_json::from_str(raw).map_err(|e| format!("Invalid JSON for {name}: {e}"))
    } else {
        Ok(value.clone())
    }
}

fn snake_to_camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

async fn auth(a: &Map<String, Value>, scopes: &[&str]) -> Result<GoogleSession, String> {
    let session = match GoogleSession::authenticate(scopes).await {
        Ok(session) => session,
        Err(error)
            if error.contains("do not include the required Google scopes")
                && scopes == [DOCS_READ] =>
        {
            GoogleSession::authenticate(&[DOCS_WRITE]).await?
        }
        Err(error)
            if error.contains("do not include the required Google scopes")
                && scopes == [DRIVE_READ] =>
        {
            GoogleSession::authenticate(&[comments::DRIVE_FULL_SCOPE]).await?
        }
        Err(error) => return Err(error),
    };
    if let Some(supplied) = opt_str(a, "user_google_email")?
        && !supplied.eq_ignore_ascii_case(&session.user_email)
    {
        return Err("user_google_email does not match the configured USER_GOOGLE_EMAIL".into());
    }
    Ok(session)
}

fn path(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}
fn max_content_bytes() -> usize {
    env_limit("WORKSPACE_MCP_DOCS_MAX_BYTES", MAX_CONTENT_BYTES_DEFAULT)
}
fn max_image_bytes() -> usize {
    env_limit("WORKSPACE_MCP_IMAGE_MAX_BYTES", MAX_IMAGE_BYTES_DEFAULT)
}
fn env_limit(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
fn doc_link(id: &str) -> String {
    format!("https://docs.google.com/document/d/{id}/edit")
}

fn validate_document_id(id: &str) -> Result<(), String> {
    if id.len() < 20
        || id.len() > 512
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid document ID. Expected a 20-512 character Google file ID.".into());
    }
    Ok(())
}
fn validate_drive_id(id: &str, field: &str) -> Result<(), String> {
    if id == "root"
        || (!id.is_empty()
            && id.len() <= 512
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    {
        Ok(())
    } else {
        Err(format!("Invalid {field}"))
    }
}
fn validate_text(text: &str, field: &str) -> Result<(), String> {
    if text.chars().count() <= MAX_TEXT_CHARS {
        Ok(())
    } else {
        Err(format!(
            "{field} exceeds the {MAX_TEXT_CHARS} character limit"
        ))
    }
}
fn validate_range(start: i64, end: i64) -> Result<(), String> {
    if start < 0 {
        Err("start_index must be >= 0".into())
    } else if end <= start {
        Err("end_index must be greater than start_index".into())
    } else {
        Ok(())
    }
}
fn extract_document_id(raw: &str) -> String {
    raw.split("/d/")
        .nth(1)
        .and_then(|s| s.split(['/', '?', '#']).next())
        .unwrap_or(raw)
        .to_owned()
}
fn utf16_len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

async fn docs_get(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    query: &[(&str, &str)],
) -> Result<Value, String> {
    validate_document_id(id)?;
    session
        .send_json(
            tool,
            session
                .request(Method::GET, &format!("{DOCS_API}/{}", path(id)))
                .query(query),
        )
        .await
}
async fn docs_batch(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    requests: Vec<Value>,
) -> Result<Value, String> {
    validate_document_id(id)?;
    if requests.is_empty() {
        return Err("No typed batchUpdate requests were generated.".into());
    }
    session
        .send_json(
            tool,
            session
                .request(
                    Method::POST,
                    &format!("{DOCS_API}/{}:batchUpdate", path(id)),
                )
                .json(&json!({"requests": requests})),
        )
        .await
}
async fn drive_metadata(session: &GoogleSession, tool: &str, id: &str) -> Result<Value, String> {
    validate_document_id(id)?;
    session
        .send_json(
            tool,
            session
                .request(Method::GET, &format!("{DRIVE_API}/{}", path(id)))
                .query(&[
                    ("fields", "id,name,mimeType,webViewLink,parents"),
                    ("supportsAllDrives", "true"),
                ]),
        )
        .await
}

async fn search_docs(a: &Map<String, Value>) -> Result<String, String> {
    let query = req_str(a, "query")?;
    let page_size = int_or(a, "page_size", 10)?;
    if !(1..=1000).contains(&page_size) {
        return Err("page_size must be between 1 and 1000".into());
    }
    let session = auth(a, &[DRIVE_READ]).await?;
    let escaped = query.replace('\\', "\\\\").replace('\'', "\\'");
    let q = format!("name contains '{escaped}' and mimeType='{GOOGLE_DOC_MIME}' and trashed=false");
    let value = session
        .send_json(
            "search_docs",
            session.request(Method::GET, DRIVE_API).query(&[
                ("q", q.as_str()),
                ("pageSize", &page_size.to_string()),
                (
                    "fields",
                    "files(id,name,createdTime,modifiedTime,webViewLink)",
                ),
                ("supportsAllDrives", "true"),
                ("includeItemsFromAllDrives", "true"),
            ]),
        )
        .await?;
    let files = arr(&value, "files");
    if files.is_empty() {
        return Ok(format!("No Google Docs found matching '{query}'."));
    }
    let mut out = vec![format!(
        "Found {} Google Docs matching '{query}':",
        files.len()
    )];
    for f in files {
        out.push(format!(
            "- {} (ID: {}) Modified: {} Link: {}",
            strv(f, "name", ""),
            strv(f, "id", ""),
            display(f.get("modifiedTime")),
            display(f.get("webViewLink"))
        ));
    }
    Ok(out.join("\n"))
}

async fn get_doc_content(a: &Map<String, Value>) -> Result<String, String> {
    let id = extract_document_id(&req_str(a, "document_id")?);
    validate_document_id(&id)?;
    let mode = string_or(a, "suggestions_view_mode", "DEFAULT_FOR_CURRENT_ACCESS")?;
    validate_suggestion_mode(&mode)?;
    let drive = auth(a, &[DRIVE_READ]).await?;
    let meta = drive_metadata(&drive, "get_doc_content", &id).await?;
    let name = strv(&meta, "name", "Unknown File");
    let mime = strv(&meta, "mimeType", "");
    let link = strv(&meta, "webViewLink", "#");
    let body = if mime == GOOGLE_DOC_MIME {
        let docs = auth(a, &[DOCS_READ]).await?;
        let doc = docs_get(
            &docs,
            "get_doc_content",
            &id,
            &[
                ("includeTabsContent", "true"),
                ("suggestionsViewMode", &mode),
            ],
        )
        .await?;
        doc_plain_text(&doc)
    } else {
        let bytes = authenticated_download(
            &drive,
            "get_doc_content",
            &format!("{DRIVE_API}/{}?alt=media&supportsAllDrives=true", path(&id)),
            max_content_bytes(),
        )
        .await?;
        String::from_utf8(bytes.clone()).unwrap_or_else(|_| {
            format!(
                "[Binary or unsupported text encoding for mimeType '{mime}' - {} bytes]",
                bytes.len()
            )
        })
    };
    Ok(format!(
        "File: \"{name}\" (ID: {id}, Type: {mime})\nLink: {link}\n\n--- CONTENT ---\n{body}"
    ))
}

async fn list_docs_in_folder(a: &Map<String, Value>) -> Result<String, String> {
    let folder = string_or(a, "folder_id", "root")?;
    validate_drive_id(&folder, "folder_id")?;
    let page_size = int_or(a, "page_size", 100)?;
    if !(1..=1000).contains(&page_size) {
        return Err("page_size must be between 1 and 1000".into());
    }
    let session = auth(a, &[DRIVE_READ]).await?;
    let q = format!("'{folder}' in parents and mimeType='{GOOGLE_DOC_MIME}' and trashed=false");
    let value = session
        .send_json(
            "list_docs_in_folder",
            session.request(Method::GET, DRIVE_API).query(&[
                ("q", q.as_str()),
                ("pageSize", &page_size.to_string()),
                ("fields", "files(id,name,modifiedTime,webViewLink)"),
                ("supportsAllDrives", "true"),
                ("includeItemsFromAllDrives", "true"),
            ]),
        )
        .await?;
    let files = arr(&value, "files");
    if files.is_empty() {
        return Ok(format!("No Google Docs found in folder '{folder}'."));
    }
    let mut out = vec![format!("Found {} Docs in folder '{folder}':", files.len())];
    for f in files {
        out.push(format!(
            "- {} (ID: {}) Modified: {} Link: {}",
            strv(f, "name", ""),
            strv(f, "id", ""),
            display(f.get("modifiedTime")),
            display(f.get("webViewLink"))
        ));
    }
    Ok(out.join("\n"))
}

async fn create_doc(a: &Map<String, Value>) -> Result<String, String> {
    let title = req_str(a, "title")?;
    let content = string_or(a, "content", "")?;
    validate_text(&title, "title")?;
    validate_text(&content, "content")?;
    let session = auth(a, &[DOCS_WRITE]).await?;
    let doc = session
        .send_json(
            "create_doc",
            session
                .request(Method::POST, DOCS_API)
                .json(&json!({"title": title})),
        )
        .await?;
    let id = strv(&doc, "documentId", "");
    validate_document_id(id)?;
    if !content.is_empty() {
        docs_batch(
            &session,
            "create_doc",
            id,
            vec![insert_text_request(Some(1), &content, None, None, false)?],
        )
        .await?;
    }
    let note = if content.is_empty() {
        "Document is empty (body starts at index 1, total length 2).".to_owned()
    } else {
        format!(
            "Initial content: {} characters inserted.",
            content.chars().count()
        )
    };
    Ok(format!(
        "Created Google Doc '{title}' (ID: {id}) for {}. {note} Use batch_update_doc with end_of_segment=true to append content. Link: {}",
        session.user_email,
        doc_link(id)
    ))
}

async fn modify_doc_text(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let start = req_i64(a, "start_index")?;
    let end = opt_i64(a, "end_index")?;
    let text = opt_str(a, "text")?;
    if let Some(t) = &text {
        validate_text(t, "text")?;
    }
    let tab = opt_str(a, "tab_id")?;
    let segment = opt_str(a, "segment_id")?;
    let eos = bool_or(a, "end_of_segment", false)?;
    let style = text_style_from_map(a)?;
    if text.is_none() && style.is_none() {
        return Ok(
            "Error: Must provide either 'text' to insert/replace, or formatting parameters.".into(),
        );
    }
    if style.is_some() && end.is_none() {
        return Ok("Error: 'end_index' is required when applying formatting.".into());
    }
    if style.is_some() && eos {
        return Ok("Error: end_of_segment cannot be used when applying formatting.".into());
    }
    let mut requests = Vec::new();
    let mut operations = Vec::new();
    if let Some(text) = &text {
        if let Some(end) = end.filter(|e| *e > start) {
            if eos {
                return Ok(
                    "Error: end_of_segment cannot be combined with text replacement.".into(),
                );
            }
            validate_range(start, end)?;
            let actual = normalize_body_index(start, tab.as_deref(), segment.as_deref());
            requests.push(delete_request(
                actual,
                end,
                tab.as_deref(),
                segment.as_deref(),
            )?);
            requests.push(insert_text_request(
                Some(actual),
                text,
                tab.as_deref(),
                segment.as_deref(),
                false,
            )?);
            operations.push(format!("Replaced text from index {start} to {end}"));
        } else {
            if start < 0 {
                return Ok("Error: start_index must be >= 0".into());
            }
            let actual = normalize_body_index(start, tab.as_deref(), segment.as_deref());
            requests.push(insert_text_request(
                if eos { None } else { Some(actual) },
                text,
                tab.as_deref(),
                segment.as_deref(),
                eos,
            )?);
            operations.push(if eos {
                format!(
                    "Inserted text at end of segment '{}'",
                    segment.as_deref().unwrap_or("body")
                )
            } else {
                format!("Inserted text at index {start}")
            });
        }
    }
    if let Some((text_style, fields)) = style {
        let end = end.expect("checked above");
        validate_range(start, end)?;
        let fs = normalize_body_index(start, tab.as_deref(), segment.as_deref());
        let mut fe = end;
        if let Some(text) = &text {
            fe = fs + utf16_len(text);
            if fe <= fs {
                fe = fs + 1;
            }
        }
        requests.push(json!({"updateTextStyle":{"range":range(fs,fe,tab.as_deref(),segment.as_deref()),"textStyle":text_style,"fields":fields.join(",")}}));
        operations.push(format!("Applied formatting to range {fs}-{fe}"));
    }
    let session = auth(a, &[DOCS_WRITE]).await?;
    docs_batch(&session, "modify_doc_text", &id, requests).await?;
    Ok(format!(
        "{} in document {id}.{} Link: {}",
        operations.join("; "),
        text.as_ref()
            .map(|t| format!(" Text length: {} characters.", t.chars().count()))
            .unwrap_or_default(),
        doc_link(&id)
    ))
}

async fn find_and_replace_doc(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let find = req_str(a, "find_text")?;
    let replace = req_str(a, "replace_text")?;
    validate_text(&replace, "replace_text")?;
    if find.is_empty() {
        return Err("find_text must not be empty".into());
    }
    let request = replace_all_request(
        &find,
        &replace,
        bool_or(a, "match_case", false)?,
        opt_str(a, "tab_id")?.as_deref(),
    );
    let session = auth(a, &[DOCS_WRITE]).await?;
    let result = docs_batch(&session, "find_and_replace_doc", &id, vec![request]).await?;
    let count = result
        .pointer("/replies/0/replaceAllText/occurrencesChanged")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Ok(format!(
        "Replaced {count} occurrence(s) of '{find}' with '{replace}' in document {id}. Link: {}",
        doc_link(&id)
    ))
}

async fn insert_doc_elements(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let mut index = req_i64(a, "index")?;
    if index == 0 {
        index = 1;
    }
    if index < 0 {
        return Err("index must be non-negative".into());
    }
    let kind = req_str(a, "element_type")?;
    let (requests, description) = match kind.as_str() {
        "table" => {
            let rows = opt_i64(a, "rows")?.ok_or(
                "Error: 'rows' and 'columns' parameters are required for table insertion.",
            )?;
            let columns = opt_i64(a, "columns")?.ok_or(
                "Error: 'rows' and 'columns' parameters are required for table insertion.",
            )?;
            validate_table_dims(rows, columns)?;
            (
                vec![insert_table_request(
                    Some(index),
                    rows,
                    columns,
                    None,
                    None,
                    false,
                )?],
                format!("table ({rows}x{columns})"),
            )
        }
        "list" => {
            let lt = opt_str(a, "list_type")?.ok_or("Error: 'list_type' parameter is required for list insertion ('UNORDERED' or 'ORDERED').")?;
            let text = opt_str(a, "text")?.unwrap_or_else(|| "List item".into());
            let mut r = vec![insert_text_request(
                Some(index),
                &(text.clone() + "\n"),
                None,
                None,
                false,
            )?];
            r.extend(bullet_requests(
                index,
                index + utf16_len(&text),
                &lt,
                BulletOptions {
                    bullet_preset: None,
                    nesting: None,
                    paragraph_starts: None,
                    tab: None,
                    segment: None,
                },
            )?);
            (r, format!("{} list", lt.to_ascii_lowercase()))
        }
        "page_break" => (
            vec![location_request(
                "insertPageBreak",
                Some(index),
                None,
                None,
                false,
            )?],
            "page break".into(),
        ),
        _ => {
            return Ok(format!(
                "Error: Unsupported element type '{kind}'. Supported types: 'table', 'list', 'page_break'."
            ));
        }
    };
    let session = auth(a, &[DOCS_WRITE]).await?;
    docs_batch(&session, "insert_doc_elements", &id, requests).await?;
    Ok(format!(
        "Inserted {description} at index {index} in document {id}. Link: {}",
        doc_link(&id)
    ))
}

async fn insert_doc_image(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let source = req_str(a, "image_source")?;
    let mut index = req_i64(a, "index")?;
    if index == 0 {
        index = 1;
    }
    let width = int_or(a, "width", 0)?;
    let height = int_or(a, "height", 0)?;
    if index < 0 || width < 0 || height < 0 {
        return Err("index, width, and height must be non-negative".into());
    }
    let (uri, description) = if source.starts_with("http://") || source.starts_with("https://") {
        validate_link_url(Some(&source))?;
        let fetched = SafeFetcher::new(max_image_bytes(), Duration::from_secs(30))
            .fetch(&source)
            .await?;
        if !fetched.status.is_success() {
            return Err(format!("Image URL returned HTTP {}", fetched.status));
        }
        let mime = fetched
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !mime.starts_with("image/") {
            return Err(format!(
                "Image URL did not return image content (Content-Type: {mime})"
            ));
        }
        (source, "URL image".to_owned())
    } else {
        validate_drive_id(&source, "image_source")?;
        let drive = auth(a, &[DRIVE_READ]).await?;
        let meta = drive_metadata(&drive, "insert_doc_image", &source).await?;
        let mime = strv(&meta, "mimeType", "");
        if !mime.starts_with("image/") {
            return Ok(format!(
                "Error: File {source} is not an image (MIME type: {mime})."
            ));
        }
        (
            format!("https://drive.google.com/uc?id={}", path(&source)),
            format!("Drive file {}", strv(&meta, "name", &source)),
        )
    };
    let request = image_request(
        Some(index),
        &uri,
        nonzero(width),
        nonzero(height),
        None,
        None,
        false,
    )?;
    let docs = auth(a, &[DOCS_WRITE]).await?;
    docs_batch(&docs, "insert_doc_image", &id, vec![request]).await?;
    let size = if width != 0 || height != 0 {
        format!(
            " (size: {}x{} points)",
            if width == 0 {
                "auto".into()
            } else {
                width.to_string()
            },
            if height == 0 {
                "auto".into()
            } else {
                height.to_string()
            }
        )
    } else {
        String::new()
    };
    Ok(format!(
        "Inserted {description}{size} at index {index} in document {id}. Link: {}",
        doc_link(&id)
    ))
}

async fn update_doc_headers_footers(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let section = req_str(a, "section_type")?.to_ascii_lowercase();
    let content = req_str(a, "content")?;
    validate_text(&content, "content")?;
    let hf_type = string_or(a, "header_footer_type", "DEFAULT")?.to_ascii_uppercase();
    if !matches!(section.as_str(), "header" | "footer") {
        return Ok("Error: section_type must be 'header' or 'footer'".into());
    }
    if !matches!(
        hf_type.as_str(),
        "DEFAULT" | "FIRST_PAGE_ONLY" | "EVEN_PAGE"
    ) {
        return Ok(
            "Error: header_footer_type must be DEFAULT, FIRST_PAGE_ONLY, or EVEN_PAGE".into(),
        );
    }
    let session = auth(a, &[DOCS_WRITE]).await?;
    let mut doc = docs_get(
        &session,
        "update_doc_headers_footers",
        &id,
        &[("includeTabsContent", "true")],
    )
    .await?;
    let style_key = style_segment_key(&section, &hf_type);
    let mut segment = find_style_segment_id(&doc, style_key);
    if segment.is_none() {
        let api_type = if hf_type == "FIRST_PAGE_ONLY" {
            "DEFAULT"
        } else {
            &hf_type
        };
        let key = if section == "header" {
            "createHeader"
        } else {
            "createFooter"
        };
        let result = docs_batch(
            &session,
            "update_doc_headers_footers",
            &id,
            vec![json!({key:{"type":api_type}})],
        )
        .await?;
        segment = result
            .pointer(&format!("/replies/0/{key}/{section}Id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if segment.is_none() {
            doc = docs_get(
                &session,
                "update_doc_headers_footers",
                &id,
                &[("includeTabsContent", "true")],
            )
            .await?;
            segment = find_style_segment_id(&doc, style_key);
        }
    }
    let segment = segment.ok_or_else(|| format!("Could not locate or create {section} segment"))?;
    let mut requests = Vec::new();
    if let Some((start, end)) = segment_bounds(&doc, &section, &segment)
        && end > start + 1
    {
        requests.push(delete_request(start, end - 1, None, Some(&segment))?);
    }
    requests.push(insert_text_request(
        Some(0),
        &content,
        None,
        Some(&segment),
        false,
    )?);
    docs_batch(&session, "update_doc_headers_footers", &id, requests).await?;
    Ok(format!(
        "Successfully updated {hf_type} {section} content. Runtime: native-rust. Link: {}",
        doc_link(&id)
    ))
}

async fn batch_update_doc(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let value = container(
        a.get("operations")
            .ok_or("Missing required argument: operations")?,
        "operations",
    )?;
    let raw = value.as_array().ok_or("operations must be a list")?;
    if serde_json::to_vec(&value)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_BATCH_INPUT_BYTES
    {
        return Err(format!(
            "operations exceeds the {MAX_BATCH_INPUT_BYTES} byte aggregate limit"
        ));
    }
    if raw.is_empty() {
        return Ok("Error: No operations provided. Please provide at least one operation.".into());
    }
    if raw.len() > 1000 {
        return Err("operations exceeds the 1000 operation limit".into());
    }
    let operations = raw
        .iter()
        .enumerate()
        .map(|(i, v)| TypedOperation::parse(v).map_err(|e| format!("Operation {}: {e}", i + 1)))
        .collect::<Result<Vec<_>, _>>()?;
    let session = auth(a, &[DOCS_WRITE]).await?;
    preflight_header_footer(&session, &id, &operations).await?;
    let mut requests = Vec::new();
    let mut descriptions = Vec::new();
    for op in &operations {
        let (mut built, desc) = op.build()?;
        if requests.len().saturating_add(built.len()) > MAX_GENERATED_REQUESTS {
            return Err(format!(
                "batch expansion exceeds the {MAX_GENERATED_REQUESTS} generated request limit"
            ));
        }
        requests.append(&mut built);
        descriptions.push(desc);
    }
    let result = docs_batch(&session, "batch_update_doc", &id, requests).await?;
    let replies = arr(&result, "replies").len();
    let mut created = Vec::new();
    for reply in arr(&result, "replies") {
        for key in ["addDocumentTab", "createDocumentTab"] {
            if let Some(p) = reply.get(key).and_then(|v| v.get("tabProperties"))
                && let Some(tid) = p.get("tabId").and_then(Value::as_str)
            {
                created.push((strv(p, "title", ""), tid));
            }
        }
    }
    let length = docs_get(
        &session,
        "batch_update_doc",
        &id,
        &[("fields", "body/content(endIndex)")],
    )
    .await
    .ok()
    .and_then(|d| {
        body_content(&d)
            .last()
            .and_then(|v| v.get("endIndex"))
            .and_then(Value::as_i64)
    });
    let summary = descriptions
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let tabs = if created.is_empty() {
        String::new()
    } else {
        format!(
            " Created tabs: {}.",
            created
                .iter()
                .map(|(t, i)| format!("'{t}' (tab_id: {i})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let length_info = length
        .map(|l| format!(" Document length: {l}."))
        .unwrap_or_default();
    Ok(format!(
        "Successfully executed {} operations ({summary}){tabs} on document {id}. API replies: {replies}.{length_info} To apply formatting, call inspect_doc_structure to get exact text positions. Link: {}",
        operations.len(),
        doc_link(&id)
    ))
}

async fn inspect_doc_structure(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let detailed = bool_or(a, "detailed", false)?;
    let target = opt_str(a, "tab_id")?;
    let session = auth(a, &[DOCS_READ]).await?;
    let doc = docs_get(
        &session,
        "inspect_doc_structure",
        &id,
        &[("includeTabsContent", "true")],
    )
    .await?;
    let (content, tab_doc) = if let Some(tid) = target.as_deref() {
        let tab = find_tab(arr(&doc, "tabs"), tid)
            .ok_or_else(|| format!("Error: Tab {tid} not found in document."))?;
        let td = tab.get("documentTab").ok_or_else(|| {
            format!("Error: Tab {tid} is not a document tab and has no body content.")
        })?;
        (body_content(td).to_vec(), Some(td))
    } else if let Some(td) = first_document_tab(arr(&doc, "tabs")) {
        (body_content(td).to_vec(), Some(td))
    } else {
        (body_content(&doc).to_vec(), None)
    };
    let tables = find_tables_in(&content);
    let total_length = content
        .last()
        .and_then(|e| e.get("endIndex"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let mut result = Map::new();
    result.insert("title".into(), json!(strv(&doc, "title", "")));
    result.insert("total_length".into(), json!(total_length));
    if detailed {
        let elements = content.iter().map(element_summary).collect::<Vec<_>>();
        result.insert("statistics".into(),json!({"elements":content.len(),"tables":tables.len(),"paragraphs":content.iter().filter(|e|e.get("paragraph").is_some()).count(),"section_breaks":content.iter().filter(|e|e.get("sectionBreak").is_some()).count(),"named_ranges":tab_doc.and_then(|d|d.get("namedRanges")).or_else(||doc.get("namedRanges")).and_then(Value::as_object).map_or(0,Map::len),"has_headers":tab_doc.and_then(|d|d.get("headers")).or_else(||doc.get("headers")).and_then(Value::as_object).is_some_and(|m|!m.is_empty()),"has_footers":tab_doc.and_then(|d|d.get("footers")).or_else(||doc.get("footers")).and_then(Value::as_object).is_some_and(|m|!m.is_empty())}));
        result.insert("elements".into(), Value::Array(elements));
        if !tables.is_empty() {
            result.insert("tables".into(),Value::Array(tables.iter().enumerate().map(|(i,t)|json!({"index":i,"position":{"start":t.start,"end":t.end},"dimensions":{"rows":t.rows,"columns":t.columns},"preview":t.data.iter().take(3).collect::<Vec<_>>() })).collect()));
        }
    } else {
        result.insert("total_elements".into(), json!(content.len()));
        result.insert("tables".into(), json!(tables.len()));
        result.insert(
            "paragraphs".into(),
            json!(
                content
                    .iter()
                    .filter(|e| e.get("paragraph").is_some())
                    .count()
            ),
        );
        if !tables.is_empty() {
            result.insert("table_details".into(),Value::Array(tables.iter().enumerate().map(|(i,t)|json!({"index":i,"rows":t.rows,"columns":t.columns,"start_index":t.start,"end_index":t.end})).collect()));
        }
    }
    let segment_source = tab_doc.unwrap_or(&doc);
    add_segments(&mut result, segment_source, "headers", "header");
    add_segments(&mut result, segment_source, "footers", "footer");
    if target.is_none() {
        result.insert(
            "tabs".into(),
            Value::Array(tab_summaries(arr(&doc, "tabs"))),
        );
    } else {
        result.insert("inspected_tab_id".into(), json!(target));
    }
    Ok(format!(
        "Document structure analysis for {id}:\n\n{}\n\nLink: {}",
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?,
        doc_link(&id)
    ))
}

async fn create_table_with_data(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let data_value = container(
        a.get("table_data")
            .ok_or("Missing required argument: table_data")?,
        "table_data",
    )?;
    let rows = data_value
        .as_array()
        .ok_or("table_data must be a 2D list")?;
    validate_table_data(rows)?;
    let index = req_i64(a, "index")?;
    if index < 0 {
        return Ok("ERROR: Index must be non-negative".into());
    }
    let tab = opt_str(a, "tab_id")?;
    let bold = bool_or(a, "bold_headers", true)?;
    let header_rows = int_or(a, "header_rows", 0)?;
    if header_rows < 0 || header_rows > rows.len() as i64 {
        return Ok(format!(
            "ERROR: header_rows must be between 0 and {}",
            rows.len()
        ));
    }
    let session = auth(a, &[DOCS_WRITE]).await?;
    let columns = rows[0].as_array().map_or(0, Vec::len) as i64;
    let mut effective = index;
    let first = docs_batch(
        &session,
        "create_table_with_data",
        &id,
        vec![insert_table_request(
            Some(effective),
            rows.len() as i64,
            columns,
            tab.as_deref(),
            None,
            false,
        )?],
    )
    .await;
    if let Err(e) = first {
        if e.contains("must be less than the end index") && index > 0 {
            effective = index - 1;
            docs_batch(
                &session,
                "create_table_with_data",
                &id,
                vec![insert_table_request(
                    Some(effective),
                    rows.len() as i64,
                    columns,
                    tab.as_deref(),
                    None,
                    false,
                )?],
            )
            .await?;
        } else {
            return Ok(format!("ERROR: {e}"));
        }
    }
    let doc = docs_get(
        &session,
        "create_table_with_data",
        &id,
        &[("includeTabsContent", "true")],
    )
    .await?;
    let content = if let Some(tid) = tab.as_deref() {
        find_tab(arr(&doc, "tabs"), tid)
            .and_then(|t| t.get("documentTab"))
            .map(body_content)
            .unwrap_or_default()
    } else if let Some(tab_doc) = first_document_tab(arr(&doc, "tabs")) {
        body_content(tab_doc)
    } else {
        body_content(&doc)
    };
    let tables = find_tables_in(content);
    let table = tables
        .iter()
        .min_by_key(|t| (t.start - effective).abs())
        .ok_or("Created table could not be located")?;
    let mut requests = Vec::new();
    for (ri, row) in rows.iter().enumerate().rev() {
        for (ci, cell) in row.as_array().unwrap().iter().enumerate().rev() {
            let text = cell.as_str().unwrap();
            if !text.is_empty() {
                let idx = table
                    .cells
                    .get(ri)
                    .and_then(|r| r.get(ci))
                    .map(|c| c.insert)
                    .ok_or("Table cell index missing")?;
                requests.push(insert_text_request(
                    Some(idx),
                    text,
                    tab.as_deref(),
                    None,
                    false,
                )?);
                if bold && ri == 0 {
                    requests.push(json!({"updateTextStyle":{"range":range(idx,idx+utf16_len(text),tab.as_deref(),None),"textStyle":{"bold":true},"fields":"bold"}}));
                }
            }
        }
    }
    if header_rows > 0 {
        requests.push(json!({"pinTableHeaderRows":{"tableStartLocation":location_value(table.start,tab.as_deref(),None),"pinnedHeaderRowsCount":header_rows}}));
    }
    if !requests.is_empty()
        && let Err(e) = docs_batch(&session, "create_table_with_data", &id, requests).await
    {
        return Ok(format!(
            "PARTIAL SUCCESS: Table created but data/style population failed: {e}. Do not retry table creation. Table: {}x{}, Index: {effective}. Link: {}",
            rows.len(),
            columns,
            doc_link(&id)
        ));
    }
    Ok(format!(
        "SUCCESS: Table created and populated successfully. Table: {}x{}, Index: {effective}. Link: {}",
        rows.len(),
        columns,
        doc_link(&id)
    ))
}

async fn debug_table_structure(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let idx = int_or(a, "table_index", 0)?;
    if idx < 0 {
        return Err("table_index must be non-negative".into());
    }
    let session = auth(a, &[DOCS_READ]).await?;
    let doc = docs_get(
        &session,
        "debug_table_structure",
        &id,
        &[("includeTabsContent", "true")],
    )
    .await?;
    let content = first_document_tab(arr(&doc, "tabs"))
        .map(body_content)
        .unwrap_or_else(|| body_content(&doc));
    let tables = find_tables_in(content);
    let Some(t) = tables.get(idx as usize) else {
        return Ok(format!(
            "Error: Table index {idx} not found. Document has {} table(s).",
            tables.len()
        ));
    };
    let cells=t.cells.iter().enumerate().map(|(r,row)|row.iter().enumerate().map(|(c,cell)|json!({"position":format!("({r},{c})"),"range":format!("[{}-{}]",cell.start,cell.end),"insertion_index":cell.insert,"current_content":format!("{:?}",cell.text),"content_elements_count":cell.elements})).collect::<Vec<_>>()).collect::<Vec<_>>();
    let info = json!({"table_index":idx,"dimensions":format!("{}x{}",t.rows,t.columns),"table_range":format!("[{}-{}]",t.start,t.end),"cells":cells});
    Ok(format!(
        "Table structure debug for table {idx}:\n\n{}\n\nLink: {}",
        serde_json::to_string_pretty(&info).unwrap(),
        doc_link(&id)
    ))
}

async fn export_doc_to_pdf(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let drive = auth(a, &[DRIVE_FILE]).await?;
    let meta = drive_metadata(&drive, "export_doc_to_pdf", &id).await?;
    let original = strv(&meta, "name", "Unknown Document");
    let mime = strv(&meta, "mimeType", "");
    let original_link = strv(&meta, "webViewLink", "#");
    if mime != GOOGLE_DOC_MIME {
        return Ok(format!(
            "Error: File '{original}' is not a Google Doc (MIME type: {mime}). Only native Google Docs can be exported to PDF."
        ));
    }
    let export_url = format!(
        "{DRIVE_API}/{}/export?mimeType=application%2Fpdf",
        path(&id)
    );
    let pdf = match authenticated_download(
        &drive,
        "export_doc_to_pdf",
        &export_url,
        max_content_bytes(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return Ok(format!("Error: Failed to export document to PDF: {e}")),
    };
    let mut filename = opt_str(a, "pdf_filename")?.unwrap_or_else(|| format!("{original}_PDF.pdf"));
    if !filename.to_ascii_lowercase().ends_with(".pdf") {
        filename.push_str(".pdf");
    }
    validate_filename(&filename)?;
    let folder = opt_str(a, "folder_id")?;
    if let Some(f) = &folder {
        validate_drive_id(f, "folder_id")?;
    }
    let metadata = json!({"name":filename,"mimeType":"application/pdf","parents":folder.iter().collect::<Vec<_>>()});
    let boundary = "workspace_mcp_native_boundary";
    let mut body=format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: application/pdf\r\n\r\n",metadata).into_bytes();
    body.extend_from_slice(&pdf);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    let upload = drive
        .send_json(
            "export_doc_to_pdf",
            drive
                .request(Method::POST, DRIVE_UPLOAD_API)
                .query(&[
                    ("uploadType", "multipart"),
                    ("fields", "id,name,webViewLink,parents"),
                    ("supportsAllDrives", "true"),
                ])
                .header(
                    "Content-Type",
                    format!("multipart/related; boundary={boundary}"),
                )
                .body(body),
        )
        .await;
    let uploaded = match upload {
        Ok(v) => v,
        Err(e) => {
            return Ok(format!(
                "Error: Failed to upload PDF to Drive: {e}. PDF was generated successfully ({} bytes) but could not be saved to Drive.",
                pdf.len()
            ));
        }
    };
    let uid = strv(&uploaded, "id", "");
    let ulink = strv(&uploaded, "webViewLink", "#");
    let folder_info = folder
        .as_ref()
        .map(|f| format!(" in folder {f}"))
        .or_else(|| {
            arr(&uploaded, "parents")
                .first()
                .and_then(Value::as_str)
                .map(|f| format!(" in folder {f}"))
        })
        .unwrap_or_default();
    Ok(format!(
        "Successfully exported '{original}' to PDF and saved to Drive as '{filename}' (ID: {uid}, {} bytes){folder_info}. PDF: {ulink} | Original: {original_link}",
        pdf.len()
    ))
}

async fn update_paragraph_style(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let start = req_i64(a, "start_index")?;
    let end = req_i64(a, "end_index")?;
    validate_range(start, end)?;
    let tab = opt_str(a, "tab_id")?;
    let segment = opt_str(a, "segment_id")?;
    if a.get("heading_level").is_some() && a.get("named_style_type").is_some() {
        return Ok(
            "Error: heading_level and named_style_type are mutually exclusive; provide only one"
                .into(),
        );
    }
    let mut requests = Vec::new();
    let (style, fields) = paragraph_style_from_map(a)?;
    let mut summary = Vec::new();
    if !fields.is_empty() {
        requests.push(json!({"updateParagraphStyle":{"range":range(normalize_body_index(start,tab.as_deref(),segment.as_deref()),end,tab.as_deref(),segment.as_deref()),"paragraphStyle":style,"fields":fields.join(",")}}));
        summary.extend(fields);
    }
    if let Some(lt) = opt_str(a, "list_type")? {
        let nesting = opt_i64(a, "list_nesting_level")?;
        if nesting.is_some_and(|n| !(0..=8).contains(&n)) {
            return Ok("Error: list_nesting_level must be between 0 and 8".into());
        }
        requests.extend(bullet_requests(
            start,
            end,
            &lt,
            BulletOptions {
                bullet_preset: opt_str(a, "bullet_preset")?.as_deref(),
                nesting,
                paragraph_starts: None,
                tab: tab.as_deref(),
                segment: segment.as_deref(),
            },
        )?);
        summary.push(format!("{} list", lt.to_ascii_lowercase()));
    }
    if requests.is_empty() {
        return Ok(format!(
            "No paragraph style changes or list creation specified for document {id}"
        ));
    }
    let session = auth(a, &[DOCS_WRITE]).await?;
    docs_batch(&session, "update_paragraph_style", &id, requests).await?;
    Ok(format!(
        "Applied paragraph formatting ({}) to range {start}-{end} in document {id}. Link: {}",
        summary.join(", "),
        doc_link(&id)
    ))
}

async fn get_doc_as_markdown(a: &Map<String, Value>) -> Result<String, String> {
    let id = extract_document_id(&req_str(a, "document_id")?);
    validate_document_id(&id)?;
    let mode = string_or(a, "comment_mode", "inline")?;
    if !matches!(mode.as_str(), "inline" | "appendix" | "none") {
        return Ok(format!(
            "Error: comment_mode must be one of ('inline', 'appendix', 'none'), got '{mode}'"
        ));
    }
    let suggestions = string_or(a, "suggestions_view_mode", "DEFAULT_FOR_CURRENT_ACCESS")?;
    validate_suggestion_mode(&suggestions)?;
    let docs = auth(a, &[DOCS_READ]).await?;
    let doc = docs_get(
        &docs,
        "get_doc_as_markdown",
        &id,
        &[
            ("includeTabsContent", "true"),
            ("suggestionsViewMode", &suggestions),
        ],
    )
    .await?;
    let mut markdown = doc_markdown(&doc);
    if !bool_or(a, "include_comments", true)? || mode == "none" {
        return Ok(markdown);
    }
    let drive = auth(a, &[DRIVE_READ]).await?;
    let comments = fetch_raw_comments(
        &drive,
        "get_doc_as_markdown",
        &id,
        bool_or(a, "include_resolved", false)?,
    )
    .await?;
    if comments.is_empty() {
        return Ok(markdown);
    }
    if mode == "inline" {
        markdown = format_comments_inline(markdown, &comments);
    } else {
        markdown = format!(
            "{}\n\n{}",
            markdown.trim_end(),
            format_comments_appendix(&comments)
        );
    }
    Ok(markdown)
}

async fn manage_doc_tab(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let action = req_str(a, "action")?;
    let tab = opt_str(a, "tab_id")?;
    let title = opt_str(a, "title")?;
    let index = opt_i64(a, "index")?;
    let parent = opt_str(a, "parent_tab_id")?;
    let markdown = opt_str(a, "markdown_text")?;
    let session = auth(a, &[DOCS_WRITE]).await?;
    let link = doc_link(&id);
    let (message, tab_id, count) = match action.as_str() {
        "create" => {
            let title = title.ok_or("'title' is required for the 'create' action.")?;
            let index = index.ok_or("'index' is required for the 'create' action.")?;
            if index < 0 {
                return Err("index must be non-negative".into());
            }
            let mut props = json!({"title":title,"index":index});
            if let Some(p) = parent.as_deref() {
                props["parentTabId"] = json!(p);
            }
            let result = docs_batch(
                &session,
                "manage_doc_tab",
                &id,
                vec![json!({"addDocumentTab":{"tabProperties":props}})],
            )
            .await?;
            let new_id = result
                .pointer("/replies/0/addDocumentTab/tabProperties/tabId")
                .or_else(|| result.pointer("/replies/0/createDocumentTab/tabProperties/tabId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let mut msg = format!("Inserted tab '{title}' at index {index} in document {id}.");
            if let Some(t) = &new_id {
                msg.push_str(&format!(" Tab ID: {t}."));
            }
            if let Some(p) = parent {
                msg.push_str(&format!(" Nested under parent tab {p}."));
            }
            (msg, new_id, 1)
        }
        "delete" => {
            let tid = tab.ok_or("'tab_id' is required for the 'delete' action.")?;
            docs_batch(
                &session,
                "manage_doc_tab",
                &id,
                vec![json!({"deleteTab":{"tabId":tid}})],
            )
            .await?;
            (
                format!("Deleted tab '{tid}' from document {id}."),
                Some(tid),
                1,
            )
        }
        "rename" => {
            let tid = tab.ok_or("'tab_id' is required for the 'rename' action.")?;
            let title = title.ok_or("'title' is required for the 'rename' action.")?;
            docs_batch(&session,"manage_doc_tab",&id,vec![json!({"updateDocumentTabProperties":{"tabProperties":{"tabId":tid,"title":title},"fields":"title"}})]).await?;
            (
                format!("Renamed tab '{tid}' to '{title}' in document {id}."),
                Some(tid),
                1,
            )
        }
        "populate_from_markdown" => {
            let tid = tab.ok_or("'tab_id' is required for the 'populate_from_markdown' action.")?;
            let md = markdown
                .ok_or("'markdown_text' is required for the 'populate_from_markdown' action.")?;
            validate_text(&md, "markdown_text")?;
            let doc = docs_get(
                &session,
                "manage_doc_tab",
                &id,
                &[("includeTabsContent", "true")],
            )
            .await?;
            let td = find_tab(arr(&doc, "tabs"), &tid)
                .and_then(|t| t.get("documentTab"))
                .ok_or_else(|| format!("'{tid}' not found in document"))?;
            let end = body_content(td)
                .last()
                .and_then(|v| v.get("endIndex"))
                .and_then(Value::as_i64)
                .unwrap_or(1);
            let replace = bool_or(a, "replace_existing", true)?;
            let mut reqs = Vec::new();
            if replace && end > 2 {
                reqs.push(delete_request(1, end - 1, Some(&tid), None)?);
            }
            let start = if replace || end <= 2 { 1 } else { end - 1 };
            reqs.extend(markdown_requests(&md, Some(&tid), start)?);
            let count = reqs.len();
            if count > 0 {
                docs_batch(&session, "manage_doc_tab", &id, reqs).await?;
            }
            let msg = if count == 0 {
                format!(
                    "No changes applied to tab '{tid}' in document {id}; markdown produced no requests."
                )
            } else {
                format!(
                    "Populated tab '{tid}' in document {id} from {} characters of markdown.",
                    md.chars().count()
                )
            };
            (msg, Some(tid), count)
        }
        _ => return Err("action must be create, rename, delete, or populate_from_markdown".into()),
    };
    // The native router only accepts strings. Preserve the structured result by
    // serializing the exact object; the integration layer can decode it later.
    serde_json::to_string(&json!({"action":action,"success":true,"message":message,"tab_id":tab_id,"requests_applied":count,"link":link})).map_err(|e|e.to_string())
}

async fn list_document_comments(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let session = match comments::drive_readonly_session().await {
        Ok(session) => session,
        Err(error) if error.contains("do not include the required Google scopes") => {
            comments::drive_full_session().await?
        }
        Err(error) => return Err(error),
    };
    verify_principal(a, &session)?;
    comments::list_comments(
        &session,
        "list_document_comments",
        "document",
        &id,
        opt_i64(a, "max_comments")?,
    )
    .await
}
async fn manage_document_comment(a: &Map<String, Value>) -> Result<String, String> {
    let id = req_doc_id(a)?;
    let action = req_str(a, "action")?;
    let content = opt_str(a, "comment_content")?;
    let cid = opt_str(a, "comment_id")?;
    let parsed = CommentAction::parse(&action, content.as_deref(), cid.as_deref())?;
    let session = comments::drive_full_session().await?;
    verify_principal(a, &session)?;
    comments::manage_comment(&session, "manage_document_comment", &id, parsed).await
}
fn verify_principal(a: &Map<String, Value>, s: &GoogleSession) -> Result<(), String> {
    if let Some(e) = opt_str(a, "user_google_email")?
        && !e.eq_ignore_ascii_case(&s.user_email)
    {
        return Err("user_google_email does not match the configured USER_GOOGLE_EMAIL".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    InsertText,
    DeleteText,
    ReplaceText,
    FormatText,
    UpdateParagraphStyle,
    UpdateTableCellStyle,
    InsertTable,
    InsertTableRow,
    DeleteTableRow,
    InsertTableColumn,
    DeleteTableColumn,
    MergeTableCells,
    UnmergeTableCells,
    UpdateTableColumnProperties,
    UpdateTableRowStyle,
    PinTableHeaderRows,
    InsertPageBreak,
    InsertSectionBreak,
    FindReplace,
    CreateBulletList,
    CreateNamedRange,
    ReplaceNamedRangeContent,
    DeleteNamedRange,
    UpdateDocumentStyle,
    UpdateSectionStyle,
    CreateHeaderFooter,
    InsertImage,
    InsertDocTab,
    DeleteDocTab,
    UpdateDocTab,
}
struct TypedOperation {
    kind: OperationKind,
    fields: Map<String, Value>,
}
impl OperationKind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "insert_text" => Self::InsertText,
            "delete_text" => Self::DeleteText,
            "replace_text" => Self::ReplaceText,
            "format_text" => Self::FormatText,
            "update_paragraph_style" => Self::UpdateParagraphStyle,
            "update_table_cell_style" => Self::UpdateTableCellStyle,
            "insert_table" => Self::InsertTable,
            "insert_table_row" => Self::InsertTableRow,
            "delete_table_row" => Self::DeleteTableRow,
            "insert_table_column" => Self::InsertTableColumn,
            "delete_table_column" => Self::DeleteTableColumn,
            "merge_table_cells" => Self::MergeTableCells,
            "unmerge_table_cells" => Self::UnmergeTableCells,
            "update_table_column_properties" => Self::UpdateTableColumnProperties,
            "update_table_row_style" => Self::UpdateTableRowStyle,
            "pin_table_header_rows" => Self::PinTableHeaderRows,
            "insert_page_break" => Self::InsertPageBreak,
            "insert_section_break" => Self::InsertSectionBreak,
            "find_replace" => Self::FindReplace,
            "create_bullet_list" => Self::CreateBulletList,
            "create_named_range" => Self::CreateNamedRange,
            "replace_named_range_content" => Self::ReplaceNamedRangeContent,
            "delete_named_range" => Self::DeleteNamedRange,
            "update_document_style" => Self::UpdateDocumentStyle,
            "update_section_style" => Self::UpdateSectionStyle,
            "create_header_footer" => Self::CreateHeaderFooter,
            "insert_image" => Self::InsertImage,
            "insert_doc_tab" => Self::InsertDocTab,
            "delete_doc_tab" => Self::DeleteDocTab,
            "update_doc_tab" => Self::UpdateDocTab,
            _ => return None,
        })
    }
}
impl TypedOperation {
    fn parse(value: &Value) -> Result<Self, String> {
        let fields = value
            .as_object()
            .ok_or("operation must be an object")?
            .clone();
        let t = fields
            .get("type")
            .and_then(Value::as_str)
            .ok_or("Missing 'type' field")?;
        let kind =
            OperationKind::parse(t).ok_or_else(|| format!("Unsupported operation type: {t}"))?;
        validate_operation_fields(kind, &fields)?;
        Ok(Self { kind, fields })
    }
    fn build(&self) -> Result<(Vec<Value>, String), String> {
        let f = &self.fields;
        let tab = map_opt_str(f, "tab_id")?;
        let segment = map_opt_str(f, "segment_id")?;
        let eos = map_bool_or(f, "end_of_segment", false)?;
        use OperationKind::*;
        let pair = match self.kind {
            InsertText => (
                vec![insert_text_request(
                    map_opt_i64(f, "index")?,
                    &map_req_str(f, "text")?,
                    tab.as_deref(),
                    segment.as_deref(),
                    eos,
                )?],
                "insert text".into(),
            ),
            DeleteText => {
                let s = map_req_i64(f, "start_index")?;
                let e = map_req_i64(f, "end_index")?;
                validate_range(s, e)?;
                (
                    vec![delete_request(s, e, tab.as_deref(), segment.as_deref())?],
                    format!("delete text {s}-{e}"),
                )
            }
            ReplaceText => {
                let s = map_req_i64(f, "start_index")?;
                let e = map_req_i64(f, "end_index")?;
                validate_range(s, e)?;
                let text = map_req_str(f, "text")?;
                (
                    vec![
                        delete_request(s, e, tab.as_deref(), segment.as_deref())?,
                        insert_text_request(
                            Some(s),
                            &text,
                            tab.as_deref(),
                            segment.as_deref(),
                            false,
                        )?,
                    ],
                    format!("replace text {s}-{e}"),
                )
            }
            FormatText => {
                let s = map_req_i64(f, "start_index")?;
                let e = map_req_i64(f, "end_index")?;
                validate_range(s, e)?;
                let (style, fields) =
                    text_style_from_map(f)?.ok_or("No formatting options provided")?;
                (
                    vec![
                        json!({"updateTextStyle":{"range":range(s,e,tab.as_deref(),segment.as_deref()),"textStyle":style,"fields":fields.join(",")}}),
                    ],
                    format!("format text {s}-{e}"),
                )
            }
            UpdateParagraphStyle => {
                let s = map_req_i64(f, "start_index")?;
                let e = map_req_i64(f, "end_index")?;
                validate_range(s, e)?;
                let (style, fields) = paragraph_style_from_map(f)?;
                if fields.is_empty() {
                    return Err("No paragraph style options provided".into());
                }
                (
                    vec![
                        json!({"updateParagraphStyle":{"range":range(normalize_body_index(s,tab.as_deref(),segment.as_deref()),e,tab.as_deref(),segment.as_deref()),"paragraphStyle":style,"fields":fields.join(",")}}),
                    ],
                    format!("paragraph style {s}-{e}"),
                )
            }
            UpdateTableCellStyle => (
                vec![table_cell_style_request(f, tab.as_deref())?],
                "update table cell style".into(),
            ),
            InsertTable => {
                let r = map_req_i64(f, "rows")?;
                let c = map_req_i64(f, "columns")?;
                validate_table_dims(r, c)?;
                (
                    vec![insert_table_request(
                        map_opt_i64(f, "index")?,
                        r,
                        c,
                        tab.as_deref(),
                        segment.as_deref(),
                        eos,
                    )?],
                    format!("insert {r}x{c} table"),
                )
            }
            InsertTableRow => (
                vec![table_row_request("insertTableRow", f, tab.as_deref())?],
                "insert table row".into(),
            ),
            DeleteTableRow => (
                vec![table_row_request("deleteTableRow", f, tab.as_deref())?],
                "delete table row".into(),
            ),
            InsertTableColumn => (
                vec![table_column_request(
                    "insertTableColumn",
                    f,
                    tab.as_deref(),
                )?],
                "insert table column".into(),
            ),
            DeleteTableColumn => (
                vec![table_column_request(
                    "deleteTableColumn",
                    f,
                    tab.as_deref(),
                )?],
                "delete table column".into(),
            ),
            MergeTableCells => (
                vec![merge_request("mergeTableCells", f, tab.as_deref())?],
                "merge table cells".into(),
            ),
            UnmergeTableCells => (
                vec![merge_request("unmergeTableCells", f, tab.as_deref())?],
                "unmerge table cells".into(),
            ),
            UpdateTableColumnProperties => (
                vec![table_column_properties_request(f, tab.as_deref())?],
                "update table column properties".into(),
            ),
            UpdateTableRowStyle => (
                vec![table_row_style_request(f, tab.as_deref())?],
                "update table row style".into(),
            ),
            PinTableHeaderRows => {
                let start = map_req_i64(f, "table_start_index")?;
                let n = map_req_i64(f, "pinned_header_rows_count")?;
                if n < 0 {
                    return Err("pinned_header_rows_count must be non-negative".into());
                }
                (
                    vec![
                        json!({"pinTableHeaderRows":{"tableStartLocation":location_value(start,tab.as_deref(),None),"pinnedHeaderRowsCount":n}}),
                    ],
                    format!("pin {n} header row(s)"),
                )
            }
            InsertPageBreak => (
                vec![location_request(
                    "insertPageBreak",
                    map_opt_i64(f, "index")?,
                    tab.as_deref(),
                    None,
                    eos,
                )?],
                "insert page break".into(),
            ),
            InsertSectionBreak => {
                let st = map_string_or(f, "section_type", "NEXT_PAGE")?.to_ascii_uppercase();
                if !matches!(st.as_str(), "CONTINUOUS" | "NEXT_PAGE") {
                    return Err("section_type must be CONTINUOUS or NEXT_PAGE".into());
                }
                let mut body = json!({"sectionType":st});
                add_location(
                    &mut body,
                    map_opt_i64(f, "index")?,
                    tab.as_deref(),
                    None,
                    eos,
                )?;
                (
                    vec![json!({"insertSectionBreak":body})],
                    "insert section break".into(),
                )
            }
            FindReplace => {
                let find = map_req_str(f, "find_text")?;
                let repl = map_req_str(f, "replace_text")?;
                (
                    vec![replace_all_request(
                        &find,
                        &repl,
                        map_bool_or(f, "match_case", false)?,
                        tab.as_deref(),
                    )],
                    format!("find/replace '{find}'"),
                )
            }
            CreateBulletList => {
                let s = map_req_i64(f, "start_index")?;
                let e = map_req_i64(f, "end_index")?;
                let lt = map_string_or(f, "list_type", "UNORDERED")?;
                (
                    bullet_requests(
                        s,
                        e,
                        &lt,
                        BulletOptions {
                            bullet_preset: map_opt_str(f, "bullet_preset")?.as_deref(),
                            nesting: map_opt_i64(f, "nesting_level")?,
                            paragraph_starts: map_i64_array(f, "paragraph_start_indices")?,
                            tab: tab.as_deref(),
                            segment: segment.as_deref(),
                        },
                    )?,
                    format!("create {} list {s}-{e}", lt.to_ascii_lowercase()),
                )
            }
            CreateNamedRange => {
                let name = map_req_str(f, "name")?;
                if name.encode_utf16().count() > 256 {
                    return Err("name exceeds 256 UTF-16 code units".into());
                }
                let s = map_req_i64(f, "start_index")?;
                let e = map_req_i64(f, "end_index")?;
                (
                    vec![
                        json!({"createNamedRange":{"name":name,"range":range(s,e,tab.as_deref(),segment.as_deref())}}),
                    ],
                    "create named range".into(),
                )
            }
            ReplaceNamedRangeContent => {
                let mut b = json!({"text":map_req_str(f,"text")?});
                named_target(&mut b, f, true)?;
                add_tabs_criteria(&mut b, tab.as_deref());
                (
                    vec![json!({"replaceNamedRangeContent":b})],
                    "replace named range content".into(),
                )
            }
            DeleteNamedRange => {
                let mut b = json!({});
                named_target(&mut b, f, false)?;
                add_tabs_criteria(&mut b, tab.as_deref());
                (
                    vec![json!({"deleteNamedRange":b})],
                    "delete named range".into(),
                )
            }
            UpdateDocumentStyle => (
                vec![document_style_request(f, tab.as_deref())?],
                "update document style".into(),
            ),
            UpdateSectionStyle => (
                vec![section_style_request(f, tab.as_deref())?],
                "update section style".into(),
            ),
            CreateHeaderFooter => {
                let section = map_req_str(f, "section_type")?;
                let typ = map_string_or(f, "header_footer_type", "DEFAULT")?;
                let key = match section.as_str() {
                    "header" => "createHeader",
                    "footer" => "createFooter",
                    _ => return Err("section_type must be 'header' or 'footer'".into()),
                };
                let mut b = json!({"type":if typ=="FIRST_PAGE_ONLY"{"DEFAULT"}else{&typ}});
                if let Some(i) = map_opt_i64(f, "section_break_index")? {
                    b["sectionBreakLocation"] = location_value(i, tab.as_deref(), None);
                } else if let Some(tab_id) = tab.as_deref() {
                    // Index 0 is the leading section break for a document tab.
                    b["sectionBreakLocation"] = location_value(0, Some(tab_id), None);
                }
                (vec![json!({key:b})], format!("create {section}"))
            }
            InsertImage => {
                let uri = map_req_str(f, "image_uri")?;
                validate_link_url(Some(&uri))?;
                (
                    vec![image_request(
                        map_opt_i64(f, "index")?,
                        &uri,
                        map_opt_i64(f, "width")?,
                        map_opt_i64(f, "height")?,
                        tab.as_deref(),
                        segment.as_deref(),
                        eos,
                    )?],
                    "insert image".into(),
                )
            }
            InsertDocTab => {
                let mut p =
                    json!({"title":map_req_str(f,"title")?,"index":map_req_i64(f,"index")?});
                if let Some(v) = map_opt_str(f, "parent_tab_id")? {
                    p["parentTabId"] = json!(v);
                }
                (
                    vec![json!({"addDocumentTab":{"tabProperties":p}})],
                    "insert tab".into(),
                )
            }
            DeleteDocTab => (
                vec![json!({"deleteTab":{"tabId":map_req_str(f,"tab_id")?}})],
                "delete tab".into(),
            ),
            UpdateDocTab => (
                vec![
                    json!({"updateDocumentTabProperties":{"tabProperties":{"tabId":map_req_str(f,"tab_id")?,"title":map_req_str(f,"title")?},"fields":"title"}}),
                ],
                "rename tab".into(),
            ),
        };
        Ok(pair)
    }
}

fn validate_operation_fields(kind: OperationKind, f: &Map<String, Value>) -> Result<(), String> {
    use OperationKind::*;
    let common = &["type", "tab_id"];
    let segment = &["segment_id"];
    let allowed: &[&str] = match kind {
        InsertText => &[
            "type",
            "text",
            "index",
            "end_of_segment",
            "tab_id",
            "segment_id",
        ],
        DeleteText => &["type", "start_index", "end_index", "tab_id", "segment_id"],
        ReplaceText => &[
            "type",
            "start_index",
            "end_index",
            "text",
            "tab_id",
            "segment_id",
        ],
        FormatText => &[
            "type",
            "start_index",
            "end_index",
            "bold",
            "italic",
            "underline",
            "strikethrough",
            "font_size",
            "font_family",
            "font_weight",
            "text_color",
            "background_color",
            "link_url",
            "clear_link",
            "baseline_offset",
            "small_caps",
            "tab_id",
            "segment_id",
        ],
        UpdateParagraphStyle => &[
            "type",
            "start_index",
            "end_index",
            "heading_level",
            "alignment",
            "line_spacing",
            "indent_first_line",
            "indent_start",
            "indent_end",
            "space_above",
            "space_below",
            "named_style_type",
            "direction",
            "keep_lines_together",
            "keep_with_next",
            "avoid_widow_and_orphan",
            "page_break_before",
            "spacing_mode",
            "shading_color",
            "border_edges",
            "border_color",
            "border_width",
            "border_padding",
            "border_dash",
            "tab_id",
            "segment_id",
        ],
        UpdateTableCellStyle => &[
            "type",
            "table_start_index",
            "background_color",
            "border_color",
            "border_width",
            "padding_top",
            "padding_bottom",
            "padding_left",
            "padding_right",
            "content_alignment",
            "row_index",
            "column_index",
            "row_span",
            "column_span",
            "border_edges",
            "tab_id",
        ],
        InsertTable => &[
            "type",
            "rows",
            "columns",
            "index",
            "end_of_segment",
            "tab_id",
            "segment_id",
        ],
        InsertTableRow => &[
            "type",
            "table_start_index",
            "row_index",
            "insert_below",
            "tab_id",
        ],
        DeleteTableRow => &["type", "table_start_index", "row_index", "tab_id"],
        InsertTableColumn => &[
            "type",
            "table_start_index",
            "column_index",
            "insert_right",
            "tab_id",
        ],
        DeleteTableColumn => &["type", "table_start_index", "column_index", "tab_id"],
        MergeTableCells | UnmergeTableCells => &[
            "type",
            "table_start_index",
            "row_index",
            "column_index",
            "row_span",
            "column_span",
            "tab_id",
        ],
        UpdateTableColumnProperties => &[
            "type",
            "table_start_index",
            "column_indices",
            "width",
            "width_type",
            "tab_id",
        ],
        UpdateTableRowStyle => &[
            "type",
            "table_start_index",
            "row_indices",
            "min_row_height",
            "tab_id",
        ],
        PinTableHeaderRows => &[
            "type",
            "table_start_index",
            "pinned_header_rows_count",
            "tab_id",
        ],
        InsertPageBreak => &["type", "index", "end_of_segment", "tab_id"],
        InsertSectionBreak => &["type", "index", "end_of_segment", "section_type", "tab_id"],
        FindReplace => &["type", "find_text", "replace_text", "match_case", "tab_id"],
        CreateBulletList => &[
            "type",
            "start_index",
            "end_index",
            "list_type",
            "nesting_level",
            "paragraph_start_indices",
            "bullet_preset",
            "tab_id",
            "segment_id",
        ],
        CreateNamedRange => &[
            "type",
            "name",
            "start_index",
            "end_index",
            "tab_id",
            "segment_id",
        ],
        ReplaceNamedRangeContent => &[
            "type",
            "text",
            "named_range_id",
            "named_range_name",
            "tab_id",
        ],
        DeleteNamedRange => &["type", "named_range_id", "named_range_name", "tab_id"],
        UpdateDocumentStyle => &[
            "type",
            "background_color",
            "margin_top",
            "margin_bottom",
            "margin_left",
            "margin_right",
            "margin_header",
            "margin_footer",
            "page_width",
            "page_height",
            "page_number_start",
            "use_even_page_header_footer",
            "use_first_page_header_footer",
            "flip_page_orientation",
            "document_mode",
            "tab_id",
        ],
        UpdateSectionStyle => &[
            "type",
            "start_index",
            "end_index",
            "margin_top",
            "margin_bottom",
            "margin_left",
            "margin_right",
            "margin_header",
            "margin_footer",
            "page_number_start",
            "use_first_page_header_footer",
            "flip_page_orientation",
            "content_direction",
            "column_count",
            "column_spacing",
            "column_separator_style",
            "tab_id",
        ],
        CreateHeaderFooter => &[
            "type",
            "section_type",
            "header_footer_type",
            "section_break_index",
            "tab_id",
        ],
        InsertImage => &[
            "type",
            "image_uri",
            "index",
            "width",
            "height",
            "end_of_segment",
            "tab_id",
            "segment_id",
        ],
        InsertDocTab => &["type", "title", "index", "parent_tab_id"],
        DeleteDocTab => &["type", "tab_id"],
        UpdateDocTab => &["type", "tab_id", "title"],
    };
    let _ = (common, segment);
    for key in f.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("Unexpected field '{key}' for operation"));
        }
    }
    for (key, v) in f {
        if key == "type" {
            continue;
        }
        let is_bool = matches!(
            key.as_str(),
            "end_of_segment"
                | "bold"
                | "italic"
                | "underline"
                | "strikethrough"
                | "clear_link"
                | "small_caps"
                | "keep_lines_together"
                | "keep_with_next"
                | "avoid_widow_and_orphan"
                | "page_break_before"
                | "match_case"
                | "insert_below"
                | "insert_right"
                | "use_even_page_header_footer"
                | "use_first_page_header_footer"
                | "flip_page_orientation"
        );
        let is_int = matches!(
            key.as_str(),
            "index"
                | "start_index"
                | "end_index"
                | "rows"
                | "columns"
                | "font_weight"
                | "heading_level"
                | "row_index"
                | "column_index"
                | "row_span"
                | "column_span"
                | "table_start_index"
                | "pinned_header_rows_count"
                | "nesting_level"
                | "page_number_start"
                | "column_count"
                | "section_break_index"
                | "width"
                | "height"
        );
        let is_num = matches!(
            key.as_str(),
            "font_size"
                | "line_spacing"
                | "indent_first_line"
                | "indent_start"
                | "indent_end"
                | "space_above"
                | "space_below"
                | "border_width"
                | "border_padding"
                | "padding_top"
                | "padding_bottom"
                | "padding_left"
                | "padding_right"
                | "min_row_height"
                | "margin_top"
                | "margin_bottom"
                | "margin_left"
                | "margin_right"
                | "margin_header"
                | "margin_footer"
                | "page_width"
                | "page_height"
                | "column_spacing"
        );
        let is_arr = matches!(
            key.as_str(),
            "border_edges" | "column_indices" | "row_indices" | "paragraph_start_indices"
        );
        if v.is_null()
            || if is_bool {
                v.is_boolean()
            } else if is_int {
                v.as_i64().is_some()
            } else if is_num {
                v.as_f64().is_some()
            } else if is_arr {
                v.as_array().is_some()
            } else {
                v.is_string()
            }
        {
        } else {
            return Err(format!("Invalid type for field: {key}"));
        }
    }
    let required: &[&str] = match kind {
        InsertText => &["text"],
        DeleteText => &["start_index", "end_index"],
        ReplaceText => &["start_index", "end_index", "text"],
        FormatText => &["start_index", "end_index"],
        UpdateParagraphStyle => &["start_index", "end_index"],
        UpdateTableCellStyle => &["table_start_index"],
        InsertTable => &["rows", "columns"],
        InsertTableRow | DeleteTableRow => &["table_start_index", "row_index"],
        InsertTableColumn | DeleteTableColumn => &["table_start_index", "column_index"],
        MergeTableCells | UnmergeTableCells => &[
            "table_start_index",
            "row_index",
            "column_index",
            "row_span",
            "column_span",
        ],
        UpdateTableColumnProperties => &["table_start_index", "column_indices"],
        UpdateTableRowStyle => &["table_start_index", "row_indices"],
        PinTableHeaderRows => &["table_start_index", "pinned_header_rows_count"],
        FindReplace => &["find_text", "replace_text"],
        CreateBulletList => &["start_index", "end_index"],
        CreateNamedRange => &["name", "start_index", "end_index"],
        ReplaceNamedRangeContent => &["text"],
        UpdateSectionStyle => &["start_index", "end_index"],
        CreateHeaderFooter => &["section_type"],
        InsertImage => &["image_uri"],
        InsertDocTab => &["title", "index"],
        DeleteDocTab => &["tab_id"],
        UpdateDocTab => &["tab_id", "title"],
        _ => &[],
    };
    for key in required {
        if !f.contains_key(*key) || f[*key].is_null() {
            return Err(format!("Missing required field: {key}"));
        }
    }
    if matches!(
        kind,
        InsertText | InsertTable | InsertPageBreak | InsertSectionBreak | InsertImage
    ) {
        let eos = map_bool_or(f, "end_of_segment", false)?;
        if eos == f.get("index").is_some_and(|v| !v.is_null()) {
            return Err("Provide exactly one of 'index' or 'end_of_segment=true'.".into());
        }
    }
    if matches!(kind, ReplaceNamedRangeContent | DeleteNamedRange) {
        let id = map_opt_str(f, "named_range_id")?.is_some();
        let name = map_opt_str(f, "named_range_name")?.is_some();
        if id == name {
            return Err("Provide exactly one of 'named_range_id' or 'named_range_name'.".into());
        }
    }
    Ok(())
}

async fn preflight_header_footer(
    session: &GoogleSession,
    id: &str,
    ops: &[TypedOperation],
) -> Result<(), String> {
    if !ops
        .iter()
        .any(|o| o.kind == OperationKind::CreateHeaderFooter)
    {
        return Ok(());
    }
    let doc = docs_get(
        session,
        "batch_update_doc",
        id,
        &[("includeTabsContent", "true")],
    )
    .await?;
    for op in ops
        .iter()
        .filter(|o| o.kind == OperationKind::CreateHeaderFooter)
    {
        let section = map_req_str(&op.fields, "section_type")?;
        let typ = map_string_or(&op.fields, "header_footer_type", "DEFAULT")?;
        let key = style_segment_key(&section, &typ);
        let existing = if let Some(tab_id) = map_opt_str(&op.fields, "tab_id")? {
            find_tab(arr(&doc, "tabs"), &tab_id)
                .and_then(|tab| tab.get("documentTab"))
                .and_then(|tab_doc| tab_doc.pointer(&format!("/documentStyle/{key}")))
        } else {
            doc.pointer(&format!("/documentStyle/{key}"))
        };
        if map_opt_i64(&op.fields, "section_break_index")?.is_none() && existing.is_some() {
            return Err("Batch operation failed: the requested header/footer already exists. For normal header or footer text, use update_doc_headers_footers instead of batch_update_doc with create_header_footer.".into());
        }
    }
    Ok(())
}

fn insert_text_request(
    index: Option<i64>,
    text: &str,
    tab: Option<&str>,
    segment: Option<&str>,
    eos: bool,
) -> Result<Value, String> {
    validate_text(text, "text")?;
    let mut body = json!({"text":text});
    add_location(&mut body, index, tab, segment, eos)?;
    Ok(json!({"insertText":body}))
}
fn insert_table_request(
    index: Option<i64>,
    rows: i64,
    columns: i64,
    tab: Option<&str>,
    segment: Option<&str>,
    eos: bool,
) -> Result<Value, String> {
    let mut body = json!({"rows":rows,"columns":columns});
    add_location(&mut body, index, tab, segment, eos)?;
    Ok(json!({"insertTable":body}))
}
fn location_request(
    key: &str,
    index: Option<i64>,
    tab: Option<&str>,
    segment: Option<&str>,
    eos: bool,
) -> Result<Value, String> {
    let mut body = json!({});
    add_location(&mut body, index, tab, segment, eos)?;
    Ok(json!({key:body}))
}
fn add_location(
    body: &mut Value,
    index: Option<i64>,
    tab: Option<&str>,
    segment: Option<&str>,
    eos: bool,
) -> Result<(), String> {
    if eos {
        let mut l = json!({});
        if let Some(v) = tab {
            l["tabId"] = json!(v);
        }
        if let Some(v) = segment {
            l["segmentId"] = json!(v);
        }
        body["endOfSegmentLocation"] = l;
        return Ok(());
    }
    let i = index.ok_or("index is required unless end_of_segment=true")?;
    if i < 0 {
        return Err("index must be non-negative".into());
    }
    body["location"] = location_value(i, tab, segment);
    Ok(())
}
fn location_value(index: i64, tab: Option<&str>, segment: Option<&str>) -> Value {
    let mut l = json!({"index":index});
    if let Some(v) = tab {
        l["tabId"] = json!(v);
    }
    if let Some(v) = segment {
        l["segmentId"] = json!(v);
    }
    l
}
fn range(start: i64, end: i64, tab: Option<&str>, segment: Option<&str>) -> Value {
    let mut r = json!({"startIndex":start,"endIndex":end});
    if let Some(v) = tab {
        r["tabId"] = json!(v);
    }
    if let Some(v) = segment {
        r["segmentId"] = json!(v);
    }
    r
}
fn delete_request(
    start: i64,
    end: i64,
    tab: Option<&str>,
    segment: Option<&str>,
) -> Result<Value, String> {
    validate_range(start, end)?;
    Ok(json!({"deleteContentRange":{"range":range(start,end,tab,segment)}}))
}
fn replace_all_request(find: &str, replace: &str, case: bool, tab: Option<&str>) -> Value {
    let mut b = json!({"containsText":{"text":find,"matchCase":case},"replaceText":replace});
    add_tabs_criteria(&mut b, tab);
    json!({"replaceAllText":b})
}
fn add_tabs_criteria(body: &mut Value, tab: Option<&str>) {
    if let Some(v) = tab {
        body["tabsCriteria"] = json!({"tabIds":[v]});
    }
}
fn image_request(
    index: Option<i64>,
    uri: &str,
    width: Option<i64>,
    height: Option<i64>,
    tab: Option<&str>,
    segment: Option<&str>,
    eos: bool,
) -> Result<Value, String> {
    validate_link_url(Some(uri))?;
    let mut b = json!({"uri":uri});
    add_location(&mut b, index, tab, segment, eos)?;
    let mut size = json!({});
    if let Some(w) = width {
        if w <= 0 {
            return Err("width must be positive".into());
        }
        size["width"] = json!({"magnitude":w,"unit":"PT"});
    }
    if let Some(h) = height {
        if h <= 0 {
            return Err("height must be positive".into());
        }
        size["height"] = json!({"magnitude":h,"unit":"PT"});
    }
    if size.as_object().is_some_and(|m| !m.is_empty()) {
        b["objectSize"] = size;
    }
    Ok(json!({"insertInlineImage":b}))
}
fn normalize_body_index(i: i64, tab: Option<&str>, segment: Option<&str>) -> i64 {
    if i == 0 && tab.is_none() && segment.is_none() {
        1
    } else {
        i
    }
}

fn text_style_from_map(f: &Map<String, Value>) -> Result<Option<(Value, Vec<String>)>, String> {
    let mut s = json!({});
    let mut fields = Vec::new();
    for (key, api) in [
        ("bold", "bold"),
        ("italic", "italic"),
        ("underline", "underline"),
        ("strikethrough", "strikethrough"),
        ("small_caps", "smallCaps"),
    ] {
        if let Some(v) = f.get(key).filter(|v| !v.is_null()) {
            s[api] = v.clone();
            fields.push(api.into());
        }
    }
    if let Some(v) = map_opt_f64(f, "font_size")? {
        if !(1.0..=400.0).contains(&v) {
            return Err("font_size must be between 1 and 400".into());
        }
        s["fontSize"] = json!({"magnitude":v,"unit":"PT"});
        fields.push("fontSize".into());
    }
    let family = map_opt_str(f, "font_family")?;
    let weight = map_opt_i64(f, "font_weight")?;
    if weight.is_some() && family.is_none() {
        return Err("font_weight requires font_family".into());
    }
    if let Some(w) = weight
        && (!(100..=900).contains(&w) || w % 100 != 0)
    {
        return Err("font_weight must be 100-900 in steps of 100".into());
    }
    if family.is_some() || weight.is_some() {
        let mut wf = json!({});
        if let Some(v) = family {
            wf["fontFamily"] = json!(v);
        }
        if let Some(v) = weight {
            wf["weight"] = json!(v);
        }
        s["weightedFontFamily"] = wf;
        fields.push("weightedFontFamily".into());
    }
    for (key, api) in [
        ("text_color", "foregroundColor"),
        ("background_color", "backgroundColor"),
    ] {
        if let Some(v) = map_opt_str(f, key)? {
            s[api] = json!({"color":{"rgbColor":rgb(&v,key)?}});
            fields.push(api.into());
        }
    }
    let link = map_opt_str(f, "link_url")?;
    let clear = map_opt_bool(f, "clear_link")?.unwrap_or(false);
    if link.is_some() && clear {
        return Err("link_url and clear_link cannot both be provided".into());
    }
    if let Some(v) = link {
        validate_link_url(Some(&v))?;
        s["link"] = json!({"url":v});
        fields.push("link".into());
    } else if clear {
        fields.push("link".into());
    }
    if let Some(v) = map_opt_str(f, "baseline_offset")? {
        let u = v.to_ascii_uppercase();
        if !matches!(u.as_str(), "NONE" | "SUPERSCRIPT" | "SUBSCRIPT") {
            return Err("baseline_offset must be NONE, SUPERSCRIPT, or SUBSCRIPT".into());
        }
        s["baselineOffset"] = json!(u);
        fields.push("baselineOffset".into());
    }
    Ok((!fields.is_empty()).then_some((s, fields)))
}

fn paragraph_style_from_map(f: &Map<String, Value>) -> Result<(Value, Vec<String>), String> {
    let mut s = json!({});
    let mut fields = Vec::new();
    let named = map_opt_str(f, "named_style_type")?;
    let heading = map_opt_i64(f, "heading_level")?;
    if named.is_some() && heading.is_some() {
        return Err("heading_level and named_style_type are mutually exclusive".into());
    }
    if let Some(v) = named {
        let allowed = [
            "NORMAL_TEXT",
            "TITLE",
            "SUBTITLE",
            "HEADING_1",
            "HEADING_2",
            "HEADING_3",
            "HEADING_4",
            "HEADING_5",
            "HEADING_6",
        ];
        if !allowed.contains(&v.as_str()) {
            return Err("Invalid named_style_type".into());
        }
        s["namedStyleType"] = json!(v);
        fields.push("namedStyleType".into());
    } else if let Some(v) = heading {
        if !(0..=6).contains(&v) {
            return Err("heading_level must be between 0 and 6".into());
        }
        s["namedStyleType"] = json!(if v == 0 {
            "NORMAL_TEXT".into()
        } else {
            format!("HEADING_{v}")
        });
        fields.push("namedStyleType".into());
    }
    enum_field(
        f,
        &mut s,
        &mut fields,
        "alignment",
        "alignment",
        &["START", "CENTER", "END", "JUSTIFIED"],
    )?;
    enum_field(
        f,
        &mut s,
        &mut fields,
        "direction",
        "direction",
        &["LEFT_TO_RIGHT", "RIGHT_TO_LEFT"],
    )?;
    enum_field(
        f,
        &mut s,
        &mut fields,
        "spacing_mode",
        "spacingMode",
        &["NEVER_COLLAPSE", "COLLAPSE_LISTS"],
    )?;
    if let Some(v) = map_opt_f64(f, "line_spacing")? {
        if v <= 0.0 {
            return Err("line_spacing must be positive".into());
        }
        s["lineSpacing"] = json!(v * 100.0);
        fields.push("lineSpacing".into());
    }
    for (key, api) in [
        ("indent_first_line", "indentFirstLine"),
        ("indent_start", "indentStart"),
        ("indent_end", "indentEnd"),
        ("space_above", "spaceAbove"),
        ("space_below", "spaceBelow"),
    ] {
        if let Some(v) = map_opt_f64(f, key)? {
            s[api] = json!({"magnitude":v,"unit":"PT"});
            fields.push(api.into());
        }
    }
    for (key, api) in [
        ("keep_lines_together", "keepLinesTogether"),
        ("keep_with_next", "keepWithNext"),
        ("avoid_widow_and_orphan", "avoidWidowAndOrphan"),
        ("page_break_before", "pageBreakBefore"),
    ] {
        if let Some(v) = map_opt_bool(f, key)? {
            s[api] = json!(v);
            fields.push(api.into());
        }
    }
    if let Some(v) = map_opt_str(f, "shading_color")? {
        s["shading"] = json!({"backgroundColor":{"color":{"rgbColor":rgb(&v,"shading_color")?}}});
        fields.push("shading".into());
    }
    let edges = map_string_array(f, "border_edges")?;
    let border_requested = edges.is_some()
        || [
            "border_color",
            "border_width",
            "border_padding",
            "border_dash",
        ]
        .iter()
        .any(|k| f.get(*k).is_some_and(|v| !v.is_null()));
    if border_requested {
        let selected = edges
            .unwrap_or_else(|| vec!["top".into(), "bottom".into(), "left".into(), "right".into()]);
        if selected.is_empty() {
            return Err("border_edges must contain at least one edge".into());
        }
        let color = map_opt_str(f, "border_color")?.unwrap_or_else(|| "#000000".into());
        let width = map_opt_f64(f, "border_width")?.unwrap_or(1.0);
        let padding = map_opt_f64(f, "border_padding")?.unwrap_or(4.0);
        let dash = map_opt_str(f, "border_dash")?
            .unwrap_or_else(|| "SOLID".into())
            .to_ascii_uppercase();
        if !matches!(dash.as_str(), "SOLID" | "DOT" | "DASH") {
            return Err("border_dash must be SOLID, DOT, or DASH".into());
        }
        let b = json!({"color":{"color":{"rgbColor":rgb(&color,"border_color")?}},"width":{"magnitude":width,"unit":"PT"},"padding":{"magnitude":padding,"unit":"PT"},"dashStyle":dash});
        for edge in selected {
            let api = match edge.to_ascii_lowercase().as_str() {
                "top" => "borderTop",
                "bottom" => "borderBottom",
                "left" => "borderLeft",
                "right" => "borderRight",
                "between" => "borderBetween",
                _ => return Err("Invalid paragraph border edge".into()),
            };
            s[api] = b.clone();
            fields.push(api.into());
        }
    }
    Ok((s, fields))
}

fn enum_field(
    f: &Map<String, Value>,
    s: &mut Value,
    fields: &mut Vec<String>,
    key: &str,
    api: &str,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(v) = map_opt_str(f, key)? {
        let u = v.to_ascii_uppercase();
        if !allowed.contains(&u.as_str()) {
            return Err(format!("{key} must be one of: {}", allowed.join(", ")));
        }
        s[api] = json!(u);
        fields.push(api.into());
    }
    Ok(())
}
fn rgb(value: &str, name: &str) -> Result<Value, String> {
    if value.len() != 7
        || !value.starts_with('#')
        || !value[1..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(format!("{name} must be a hex string like '#RRGGBB'"));
    }
    let c = |r: std::ops::Range<usize>| u8::from_str_radix(&value[r], 16).unwrap() as f64 / 255.0;
    Ok(json!({"red":c(1..3),"green":c(3..5),"blue":c(5..7)}))
}

fn table_cell_style_request(f: &Map<String, Value>, tab: Option<&str>) -> Result<Value, String> {
    let start = map_req_i64(f, "table_start_index")?;
    let mut style = json!({});
    let mut fields = Vec::new();
    if let Some(v) = map_opt_str(f, "background_color")? {
        style["backgroundColor"] = json!({"color":{"rgbColor":rgb(&v,"background_color")?}});
        fields.push("backgroundColor".to_owned());
    }
    for (key, api) in [
        ("padding_top", "paddingTop"),
        ("padding_bottom", "paddingBottom"),
        ("padding_left", "paddingLeft"),
        ("padding_right", "paddingRight"),
    ] {
        if let Some(v) = map_opt_f64(f, key)? {
            style[api] = json!({"magnitude":v,"unit":"PT"});
            fields.push(api.into());
        }
    }
    enum_field(
        f,
        &mut style,
        &mut fields,
        "content_alignment",
        "contentAlignment",
        &["TOP", "MIDDLE", "BOTTOM"],
    )?;
    let edges = map_string_array(f, "border_edges")?;
    if edges.is_some() || f.get("border_color").is_some() || f.get("border_width").is_some() {
        let color = map_opt_str(f, "border_color")?.unwrap_or_else(|| "#000000".into());
        let width = map_opt_f64(f, "border_width")?.unwrap_or(1.0);
        let border = json!({"dashStyle":"SOLID","width":{"magnitude":width,"unit":"PT"},"color":{"color":{"rgbColor":rgb(&color,"border_color")?}}});
        for e in edges
            .unwrap_or_else(|| vec!["top".into(), "bottom".into(), "left".into(), "right".into()])
        {
            let api = match e.as_str() {
                "top" => "borderTop",
                "bottom" => "borderBottom",
                "left" => "borderLeft",
                "right" => "borderRight",
                _ => return Err("Invalid table border edge".into()),
            };
            style[api] = border.clone();
            fields.push(api.into());
        }
    }
    if fields.is_empty() {
        return Err("No table cell style options provided".into());
    }
    let loc = location_value(start, tab, None);
    let mut body = json!({"tableCellStyle":style,"fields":fields.join(",")});
    let row = map_opt_i64(f, "row_index")?;
    let col = map_opt_i64(f, "column_index")?;
    if row.is_some()
        || col.is_some()
        || f.get("row_span").is_some()
        || f.get("column_span").is_some()
    {
        let (row, col) = (
            row.ok_or("row_index and column_index are required together")?,
            col.ok_or("row_index and column_index are required together")?,
        );
        let rs = map_opt_i64(f, "row_span")?.unwrap_or(1);
        let cs = map_opt_i64(f, "column_span")?.unwrap_or(1);
        if row < 0 || col < 0 || rs <= 0 || cs <= 0 {
            return Err("Invalid table cell range".into());
        }
        body["tableRange"] = json!({"tableCellLocation":{"tableStartLocation":loc,"rowIndex":row,"columnIndex":col},"rowSpan":rs,"columnSpan":cs});
    } else {
        body["tableStartLocation"] = loc;
    }
    Ok(json!({"updateTableCellStyle":body}))
}
fn table_row_request(
    key: &str,
    f: &Map<String, Value>,
    tab: Option<&str>,
) -> Result<Value, String> {
    let loc = json!({"tableStartLocation":location_value(map_req_i64(f,"table_start_index")?,tab,None),"rowIndex":map_req_i64(f,"row_index")?,"columnIndex":0});
    let mut b = json!({"tableCellLocation":loc});
    if key == "insertTableRow" {
        b["insertBelow"] = json!(map_bool_or(f, "insert_below", true)?);
    }
    Ok(json!({key:b}))
}
fn table_column_request(
    key: &str,
    f: &Map<String, Value>,
    tab: Option<&str>,
) -> Result<Value, String> {
    let loc = json!({"tableStartLocation":location_value(map_req_i64(f,"table_start_index")?,tab,None),"rowIndex":0,"columnIndex":map_req_i64(f,"column_index")?});
    let mut b = json!({"tableCellLocation":loc});
    if key == "insertTableColumn" {
        b["insertRight"] = json!(map_bool_or(f, "insert_right", true)?);
    }
    Ok(json!({key:b}))
}
fn merge_request(key: &str, f: &Map<String, Value>, tab: Option<&str>) -> Result<Value, String> {
    let r = map_req_i64(f, "row_span")?;
    let c = map_req_i64(f, "column_span")?;
    if r <= 0 || c <= 0 {
        return Err("row_span and column_span must be positive".into());
    }
    Ok(
        json!({key:{"tableRange":{"tableCellLocation":{"tableStartLocation":location_value(map_req_i64(f,"table_start_index")?,tab,None),"rowIndex":map_req_i64(f,"row_index")?,"columnIndex":map_req_i64(f,"column_index")?},"rowSpan":r,"columnSpan":c}}}),
    )
}
fn table_column_properties_request(
    f: &Map<String, Value>,
    tab: Option<&str>,
) -> Result<Value, String> {
    let indices = map_i64_array(f, "column_indices")?.ok_or("column_indices is required")?;
    if indices.is_empty() || indices.iter().any(|i| *i < 0) {
        return Err("column_indices must contain non-negative integers".into());
    }
    let mut p = json!({});
    let mut fields = Vec::new();
    if let Some(v) = map_opt_f64(f, "width")? {
        if v <= 0.0 {
            return Err("width must be positive".into());
        }
        p["width"] = json!({"magnitude":v,"unit":"PT"});
        fields.push("width");
    }
    if let Some(v) = map_opt_str(f, "width_type")? {
        if !matches!(v.as_str(), "FIXED_WIDTH" | "EVENLY_DISTRIBUTED") {
            return Err("Invalid width_type".into());
        }
        p["widthType"] = json!(v);
        fields.push("widthType");
    }
    if fields.is_empty() {
        return Err("update_table_column_properties requires width or width_type".into());
    }
    Ok(
        json!({"updateTableColumnProperties":{"tableStartLocation":location_value(map_req_i64(f,"table_start_index")?,tab,None),"columnIndices":indices,"tableColumnProperties":p,"fields":fields.join(",")}}),
    )
}
fn table_row_style_request(f: &Map<String, Value>, tab: Option<&str>) -> Result<Value, String> {
    let rows = map_i64_array(f, "row_indices")?.ok_or("row_indices is required")?;
    if rows.is_empty() || rows.iter().any(|i| *i < 0) {
        return Err("row_indices must contain non-negative integers".into());
    }
    let h = map_opt_f64(f, "min_row_height")?
        .ok_or("update_table_row_style requires min_row_height")?;
    if h < 0.0 {
        return Err("min_row_height must be non-negative".into());
    }
    Ok(
        json!({"updateTableRowStyle":{"tableStartLocation":location_value(map_req_i64(f,"table_start_index")?,tab,None),"rowIndices":rows,"tableRowStyle":{"minRowHeight":{"magnitude":h,"unit":"PT"}},"fields":"minRowHeight"}}),
    )
}
fn document_style_request(f: &Map<String, Value>, tab: Option<&str>) -> Result<Value, String> {
    let mut s = json!({});
    let mut fields = Vec::new();
    if let Some(v) = map_opt_str(f, "background_color")? {
        s["background"] = json!({"color":{"color":{"rgbColor":rgb(&v,"background_color")?}}});
        fields.push("background");
    }
    for (key, api) in [
        ("margin_top", "marginTop"),
        ("margin_bottom", "marginBottom"),
        ("margin_left", "marginLeft"),
        ("margin_right", "marginRight"),
        ("margin_header", "marginHeader"),
        ("margin_footer", "marginFooter"),
        ("page_width", "pageSize.width"),
        ("page_height", "pageSize.height"),
    ] {
        if let Some(v) = map_opt_f64(f, key)? {
            if api.starts_with("pageSize") {
                let part = api.split('.').nth(1).unwrap();
                s["pageSize"][part] = json!({"magnitude":v,"unit":"PT"});
            } else {
                s[api] = json!({"magnitude":v,"unit":"PT"});
            }
            fields.push(api);
        }
    }
    for (key, api) in [("page_number_start", "pageNumberStart")] {
        if let Some(v) = map_opt_i64(f, key)? {
            s[api] = json!(v);
            fields.push(api);
        }
    }
    for (key, api) in [
        ("use_even_page_header_footer", "useEvenPageHeaderFooter"),
        ("use_first_page_header_footer", "useFirstPageHeaderFooter"),
        ("flip_page_orientation", "flipPageOrientation"),
    ] {
        if let Some(v) = map_opt_bool(f, key)? {
            s[api] = json!(v);
            fields.push(api);
        }
    }
    if let Some(v) = map_opt_str(f, "document_mode")? {
        if !matches!(v.as_str(), "PAGES" | "PAGELESS") {
            return Err("document_mode must be PAGES or PAGELESS".into());
        }
        s["documentFormat"] = json!({"documentMode":v});
        fields.push("documentFormat");
    }
    if fields.is_empty() {
        return Err("No document style options provided".into());
    }
    let mut b = json!({"documentStyle":s,"fields":fields.join(",")});
    if let Some(t) = tab {
        b["tabId"] = json!(t);
    }
    Ok(json!({"updateDocumentStyle":b}))
}
fn section_style_request(f: &Map<String, Value>, tab: Option<&str>) -> Result<Value, String> {
    let start = map_req_i64(f, "start_index")?;
    let end = map_req_i64(f, "end_index")?;
    validate_range(start, end)?;
    let mut style = json!({});
    let mut fields: Vec<String> = Vec::new();
    for (key, api) in [
        ("margin_top", "marginTop"),
        ("margin_bottom", "marginBottom"),
        ("margin_left", "marginLeft"),
        ("margin_right", "marginRight"),
        ("margin_header", "marginHeader"),
        ("margin_footer", "marginFooter"),
    ] {
        if let Some(value) = map_opt_f64(f, key)? {
            style[api] = json!({"magnitude":value,"unit":"PT"});
            fields.push(api.to_owned());
        }
    }
    if let Some(value) = map_opt_i64(f, "page_number_start")? {
        style["pageNumberStart"] = json!(value);
        fields.push("pageNumberStart".into());
    }
    for (key, api) in [
        ("use_first_page_header_footer", "useFirstPageHeaderFooter"),
        ("flip_page_orientation", "flipPageOrientation"),
    ] {
        if let Some(value) = map_opt_bool(f, key)? {
            style[api] = json!(value);
            fields.push(api.to_owned());
        }
    }
    enum_field(
        f,
        &mut style,
        &mut fields,
        "content_direction",
        "contentDirection",
        &["LEFT_TO_RIGHT", "RIGHT_TO_LEFT"],
    )?;
    enum_field(
        f,
        &mut style,
        &mut fields,
        "column_separator_style",
        "columnSeparatorStyle",
        &["NONE", "BETWEEN_EACH_COLUMN"],
    )?;
    let column_count = map_opt_i64(f, "column_count")?;
    let column_spacing = map_opt_f64(f, "column_spacing")?;
    if column_count.is_some() || column_spacing.is_some() {
        let count =
            column_count.ok_or("column_count is required when specifying section columns")?;
        if !(1..=3).contains(&count) {
            return Err("column_count must be between 1 and 3".into());
        }
        let columns = (0..count)
            .map(|_| {
                column_spacing.map_or_else(
                    || json!({}),
                    |spacing| json!({"paddingEnd":{"magnitude":spacing,"unit":"PT"}}),
                )
            })
            .collect::<Vec<_>>();
        style["columnProperties"] = Value::Array(columns);
        fields.push("columnProperties".into());
    }
    if fields.is_empty() {
        return Err("No section style options provided".into());
    }
    Ok(json!({"updateSectionStyle":{
        "range":range(start,end,tab,None),
        "sectionStyle":style,
        "fields":fields.join(",")
    }}))
}
fn named_target(body: &mut Value, f: &Map<String, Value>, replace: bool) -> Result<(), String> {
    match (
        map_opt_str(f, "named_range_id")?,
        map_opt_str(f, "named_range_name")?,
    ) {
        (Some(id), None) => body["namedRangeId"] = json!(id),
        (None, Some(name)) => body[if replace { "namedRangeName" } else { "name" }] = json!(name),
        _ => return Err("Provide exactly one named range target".into()),
    }
    Ok(())
}

struct BulletOptions<'a> {
    bullet_preset: Option<&'a str>,
    nesting: Option<i64>,
    paragraph_starts: Option<Vec<i64>>,
    tab: Option<&'a str>,
    segment: Option<&'a str>,
}

fn bullet_requests(
    start: i64,
    end: i64,
    list_type: &str,
    options: BulletOptions<'_>,
) -> Result<Vec<Value>, String> {
    let BulletOptions {
        bullet_preset,
        nesting,
        paragraph_starts,
        tab,
        segment,
    } = options;
    validate_range(start, end)?;
    let lt = list_type.to_ascii_uppercase();
    if lt == "NONE" {
        return Ok(vec![
            json!({"deleteParagraphBullets":{"range":range(normalize_body_index(start,tab,segment),end,tab,segment)}}),
        ]);
    }
    if !matches!(lt.as_str(), "UNORDERED" | "ORDERED" | "CHECKBOX") {
        return Err("list_type must be UNORDERED, ORDERED, CHECKBOX, or NONE".into());
    }
    let level = nesting.unwrap_or(0);
    if !(0..=8).contains(&level) {
        return Err("nesting_level must be between 0 and 8".into());
    }
    let mut requests = Vec::new();
    let mut s = normalize_body_index(start, tab, segment);
    let mut e = end;
    if level > 0 {
        let tabs = "\t".repeat(level as usize);
        let mut indices = paragraph_starts.unwrap_or_else(|| vec![s]);
        indices.sort_unstable();
        indices.dedup();
        for (i, idx) in indices.iter().enumerate() {
            requests.push(insert_text_request(
                Some(*idx + i as i64 * level),
                &tabs,
                tab,
                segment,
                false,
            )?);
        }
        e += indices.iter().filter(|i| **i < end).count() as i64 * level;
        s += indices.iter().filter(|i| **i < start).count() as i64 * level;
    }
    let default_preset = match lt.as_str() {
        "UNORDERED" => "BULLET_DISC_CIRCLE_SQUARE",
        "CHECKBOX" => "BULLET_CHECKBOX",
        _ => "NUMBERED_DECIMAL_ALPHA_ROMAN",
    };
    let preset = bullet_preset.unwrap_or(default_preset);
    const VALID_PRESETS: &[&str] = &[
        "BULLET_DISC_CIRCLE_SQUARE",
        "BULLET_DIAMONDX_ARROW3D_SQUARE",
        "BULLET_CHECKBOX",
        "BULLET_ARROW_DIAMOND_DISC",
        "BULLET_STAR_CIRCLE_SQUARE",
        "BULLET_ARROW3D_CIRCLE_SQUARE",
        "BULLET_LEFTTRIANGLE_DIAMOND_DISC",
        "BULLET_DIAMONDX_HOLLOWDIAMOND_SQUARE",
        "BULLET_DIAMOND_CIRCLE_SQUARE",
        "NUMBERED_DECIMAL_ALPHA_ROMAN",
        "NUMBERED_DECIMAL_ALPHA_ROMAN_PARENS",
        "NUMBERED_DECIMAL_NESTED",
        "NUMBERED_UPPERALPHA_ALPHA_ROMAN",
        "NUMBERED_UPPERROMAN_UPPERALPHA_DECIMAL",
        "NUMBERED_ZERODECIMAL_ALPHA_ROMAN",
    ];
    if !VALID_PRESETS.contains(&preset) {
        return Err(format!(
            "bullet_preset must be one of: {}",
            VALID_PRESETS.join(", ")
        ));
    }
    requests.push(
        json!({"createParagraphBullets":{"range":range(s,e,tab,segment),"bulletPreset":preset}}),
    );
    Ok(requests)
}

fn logical_markdown_lines(markdown: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut paragraph = String::new();
    let mut in_fence = false;
    for raw in markdown.lines() {
        let trimmed = raw.trim();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        let block = fence
            || trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("> ")
            || markdown_list_line(trimmed)
            || matches!(trimmed, "---" | "***" | "___");
        if in_fence || block {
            if !paragraph.is_empty() {
                lines.push(std::mem::take(&mut paragraph));
            }
            lines.push(raw.to_owned());
            if fence {
                in_fence = !in_fence;
            }
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(trimmed);
        }
    }
    if !paragraph.is_empty() {
        lines.push(paragraph);
    }
    lines
}

fn markdown_requests(markdown: &str, tab: Option<&str>, start: i64) -> Result<Vec<Value>, String> {
    if markdown.trim().is_empty() {
        return Ok(Vec::new());
    }
    let lines = logical_markdown_lines(markdown);
    let mut requests = Vec::new();
    let mut cursor = start;
    let mut in_fence = false;
    let mut fence = String::new();
    for (line_index, raw) in lines.iter().enumerate() {
        if requests.len() >= MAX_GENERATED_REQUESTS {
            return Err(format!(
                "markdown expansion exceeds the {MAX_GENERATED_REQUESTS} generated request limit"
            ));
        }
        if raw.trim_start().starts_with("```") || raw.trim_start().starts_with("~~~") {
            if in_fence {
                emit_code_block(&mut requests, &mut cursor, &fence, tab)?;
                fence.clear();
                in_fence = false;
            } else {
                in_fence = true;
            }
            continue;
        }
        if in_fence {
            fence.push_str(raw);
            fence.push('\n');
            continue;
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            requests.push(insert_text_request(Some(cursor), "\n", tab, None, false)?);
            cursor += 1;
            continue;
        }

        let mut heading = None;
        let mut list_preset = None;
        let mut blockquote = false;
        let visible = if let Some(rest) = trimmed.strip_prefix("###### ") {
            heading = Some(6);
            rest
        } else if let Some(rest) = trimmed.strip_prefix("##### ") {
            heading = Some(5);
            rest
        } else if let Some(rest) = trimmed.strip_prefix("#### ") {
            heading = Some(4);
            rest
        } else if let Some(rest) = trimmed.strip_prefix("### ") {
            heading = Some(3);
            rest
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            heading = Some(2);
            rest
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            heading = Some(1);
            rest
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            list_preset = Some("BULLET_DISC_CIRCLE_SQUARE");
            rest
        } else if let Some(position) = ordered_list_prefix(trimmed) {
            list_preset = Some("NUMBERED_DECIMAL_ALPHA_ROMAN");
            &trimmed[position..]
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            blockquote = true;
            rest
        } else {
            trimmed
        };

        let (mut text, styles) = parse_inline_markdown(visible, cursor, tab)?;
        text.push('\n');
        let begin = cursor;
        requests.push(insert_text_request(Some(cursor), &text, tab, None, false)?);
        cursor += utf16_len(&text);
        if requests.len().saturating_add(styles.len()) > MAX_GENERATED_REQUESTS {
            return Err(format!(
                "markdown expansion exceeds the {MAX_GENERATED_REQUESTS} generated request limit"
            ));
        }
        requests.extend(styles);
        if let Some(level) = heading {
            requests.push(json!({"updateParagraphStyle":{"range":range(begin,cursor,tab,None),"paragraphStyle":{"namedStyleType":format!("HEADING_{level}")},"fields":"namedStyleType"}}));
        }
        if let Some(preset) = list_preset {
            requests.push(json!({"createParagraphBullets":{"range":range(begin,cursor,tab,None),"bulletPreset":preset}}));
        }
        if blockquote {
            requests.push(json!({"updateParagraphStyle":{"range":range(begin,cursor,tab,None),"paragraphStyle":{"indentStart":{"magnitude":36,"unit":"PT"}},"fields":"indentStart"}}));
        }
        let next_is_same_list = list_preset.is_some()
            && lines
                .get(line_index + 1)
                .is_some_and(|line| markdown_list_line(line.trim()));
        if !next_is_same_list {
            requests.push(insert_text_request(Some(cursor), "\n", tab, None, false)?);
            cursor += 1;
        }
    }
    if in_fence {
        emit_code_block(&mut requests, &mut cursor, &fence, tab)?;
    }
    Ok(requests)
}

fn emit_code_block(
    requests: &mut Vec<Value>,
    cursor: &mut i64,
    content: &str,
    tab: Option<&str>,
) -> Result<(), String> {
    let text = format!("{}\n", content.trim_end_matches('\n'));
    let begin = *cursor;
    requests.push(insert_text_request(Some(*cursor), &text, tab, None, false)?);
    *cursor += utf16_len(&text);
    if *cursor > begin + 1 {
        requests.push(json!({"updateTextStyle":{"range":range(begin,*cursor-1,tab,None),"textStyle":{"weightedFontFamily":{"fontFamily":"Courier New","weight":400}},"fields":"weightedFontFamily"}}));
    }
    requests.push(insert_text_request(Some(*cursor), "\n", tab, None, false)?);
    *cursor += 1;
    Ok(())
}

fn ordered_list_prefix(line: &str) -> Option<usize> {
    let separator = line.find(". ")?;
    (separator > 0 && line[..separator].bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(separator + 2)
}

fn markdown_list_line(line: &str) -> bool {
    line.starts_with("- ") || ordered_list_prefix(line).is_some()
}
fn parse_inline_markdown(
    input: &str,
    base: i64,
    tab: Option<&str>,
) -> Result<(String, Vec<Value>), String> {
    let mut out = String::new();
    let mut styles = Vec::new();
    let chars = input.chars().collect::<Vec<_>>();
    let mut i = 0;
    while i < chars.len() {
        if styles.len() >= MAX_GENERATED_REQUESTS {
            return Err(format!(
                "inline markdown expansion exceeds the {MAX_GENERATED_REQUESTS} style request limit"
            ));
        }
        if i + 1 < chars.len()
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            let pair = if chars[i] == '*' { "**" } else { "__" };
            if let Some(end) = find_pair(&chars, i + 2, pair) {
                let text = chars[i + 2..end].iter().collect::<String>();
                let s = base + utf16_len(&out);
                out.push_str(&text);
                styles.push(json!({"updateTextStyle":{"range":range(s,s+utf16_len(&text),tab,None),"textStyle":{"bold":true},"fields":"bold"}}));
                i = end + 2;
                continue;
            }
        }
        if matches!(chars[i], '*' | '_') {
            let marker = chars[i];
            if let Some(end) = chars[i + 1..]
                .iter()
                .position(|c| *c == marker)
                .map(|p| i + 1 + p)
            {
                let text = chars[i + 1..end].iter().collect::<String>();
                let s = base + utf16_len(&out);
                out.push_str(&text);
                styles.push(json!({"updateTextStyle":{"range":range(s,s+utf16_len(&text),tab,None),"textStyle":{"italic":true},"fields":"italic"}}));
                i = end + 1;
                continue;
            }
        }
        if chars[i] == '`'
            && let Some(end) = chars[i + 1..]
                .iter()
                .position(|c| *c == '`')
                .map(|p| i + 1 + p)
        {
            let text = chars[i + 1..end].iter().collect::<String>();
            let s = base + utf16_len(&out);
            out.push_str(&text);
            styles.push(json!({"updateTextStyle":{"range":range(s,s+utf16_len(&text),tab,None),"textStyle":{"weightedFontFamily":{"fontFamily":"Courier New","weight":400}},"fields":"weightedFontFamily"}}));
            i = end + 1;
            continue;
        }
        let link_open = if chars[i] == '[' {
            Some(i)
        } else if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            Some(i + 1)
        } else {
            None
        };
        if let Some(open) = link_open
            && let Some(close) = chars[open + 1..]
                .iter()
                .position(|c| *c == ']')
                .map(|p| open + 1 + p)
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = chars[close + 2..]
                .iter()
                .position(|c| *c == ')')
                .map(|p| close + 2 + p)
        {
            let label = chars[open + 1..close].iter().collect::<String>();
            let url = chars[close + 2..end].iter().collect::<String>();
            let s = base + utf16_len(&out);
            out.push_str(&label);
            if let Some(safe) = sanitize_link_url(Some(&url)) {
                styles.push(json!({"updateTextStyle":{"range":range(s,s+utf16_len(&label),tab,None),"textStyle":{"link":{"url":safe}},"fields":"link"}}));
            }
            i = end + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok((out, styles))
}
fn find_pair(chars: &[char], start: usize, pair: &str) -> Option<usize> {
    let p = pair.chars().collect::<Vec<_>>();
    (start..chars.len().saturating_sub(p.len() - 1)).find(|i| chars[*i..*i + p.len()] == p)
}

fn doc_plain_text(doc: &Value) -> String {
    let mut out = String::new();
    let tabs = arr(doc, "tabs");
    if !tabs.is_empty() {
        plain_tabs(tabs, 0, &mut out);
    } else {
        extract_elements_text(body_content(doc), &mut out, 0);
    }
    out
}
fn plain_tabs(tabs: &[Value], depth: usize, out: &mut String) {
    for tab in tabs {
        if let Some(td) = tab.get("documentTab") {
            let properties = tab.get("tabProperties").unwrap_or(&Value::Null);
            let indentation = "    ".repeat(depth);
            out.push_str(&format!(
                "\n--- TAB: {indentation}{} (ID: {}) ---\n",
                strv(properties, "title", "Untitled Tab"),
                strv(properties, "tabId", "Unknown ID")
            ));
            extract_elements_text(body_content(td), out, 0);
        }
        plain_tabs(arr(tab, "childTabs"), depth + 1, out);
    }
}
fn extract_elements_text(elements: &[Value], out: &mut String, depth: usize) {
    if depth > 5 {
        return;
    }
    for e in elements {
        if let Some(p) = e.get("paragraph") {
            for pe in arr(p, "elements") {
                if let Some(t) = pe.pointer("/textRun/content").and_then(Value::as_str) {
                    out.push_str(t);
                }
            }
        }
        if let Some(t) = e.get("table") {
            for row in arr(t, "tableRows") {
                for cell in arr(row, "tableCells") {
                    extract_elements_text(arr(cell, "content"), out, depth + 1);
                }
            }
        }
    }
}

fn doc_markdown(doc: &Value) -> String {
    let tabs = arr(doc, "tabs");
    let mut docs = Vec::new();
    collect_tab_docs(tabs, &mut docs);
    if docs.len() == 1 {
        return body_markdown(docs[0].1);
    }
    if !docs.is_empty() {
        return docs
            .into_iter()
            .map(|(t, d)| format!("# {t}\n\n{}", body_markdown(d)))
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
            + "\n";
    }
    body_markdown(doc)
}
fn collect_tab_docs<'a>(tabs: &'a [Value], out: &mut Vec<(String, &'a Value)>) {
    for t in tabs {
        if let Some(d) = t.get("documentTab") {
            out.push((
                strv(
                    t.get("tabProperties").unwrap_or(&Value::Null),
                    "title",
                    "Untitled Tab",
                )
                .into(),
                d,
            ));
        }
        collect_tab_docs(arr(t, "childTabs"), out);
    }
}
fn body_markdown(doc: &Value) -> String {
    let mut lines = Vec::new();
    let lists = doc.get("lists").unwrap_or(&Value::Null);
    for element in body_content(doc) {
        if let Some(paragraph) = element.get("paragraph") {
            let bullet = paragraph.get("bullet");
            let checklist = bullet.is_some_and(|value| {
                list_is_checklist(
                    lists,
                    strv(value, "listId", ""),
                    value
                        .get("nestingLevel")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                )
            });
            let checked = checklist && paragraph_is_checked(paragraph);
            let text = paragraph_markdown(paragraph, doc, checked);
            if text.trim().is_empty() {
                continue;
            }
            if let Some(bullet) = bullet {
                let level = bullet
                    .get("nestingLevel")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let list_id = strv(bullet, "listId", "");
                let marker = if checklist {
                    if checked { "- [x]" } else { "- [ ]" }
                } else if list_is_ordered(lists, list_id, level) {
                    "1."
                } else {
                    "-"
                };
                lines.push(format!("{}{marker} {text}", "  ".repeat(level as usize)));
            } else {
                let style = paragraph
                    .pointer("/paragraphStyle/namedStyleType")
                    .and_then(Value::as_str)
                    .unwrap_or("NORMAL_TEXT");
                let prefix = match style {
                    "TITLE" | "HEADING_1" => "# ",
                    "SUBTITLE" | "HEADING_2" => "## ",
                    "HEADING_3" => "### ",
                    "HEADING_4" => "#### ",
                    "HEADING_5" => "##### ",
                    "HEADING_6" => "###### ",
                    _ => "",
                };
                lines.push(format!("{prefix}{text}"));
                lines.push(String::new());
            }
        } else if let Some(table) = element.get("table") {
            let rows = arr(table, "tableRows");
            for (row_index, row) in rows.iter().enumerate() {
                let cells = arr(row, "tableCells")
                    .iter()
                    .map(|cell| table_cell_markdown(cell, doc).replace('|', "\\|"))
                    .collect::<Vec<_>>();
                lines.push(format!("| {} |", cells.join(" | ")));
                if row_index == 0 {
                    lines.push(format!("| {} |", vec!["---"; cells.len()].join(" | ")));
                }
            }
            lines.push(String::new());
        }
    }
    if let Some(footnotes) = doc.get("footnotes").and_then(Value::as_object)
        && !footnotes.is_empty()
    {
        lines.push(String::new());
        for (id, footnote) in footnotes {
            let text = markdown_content(arr(footnote, "content"), doc);
            lines.push(format!("[^{id}]: {text}"));
        }
    }
    lines.join("\n").trim_end().to_owned() + "\n"
}

fn list_level<'a>(lists: &'a Value, list_id: &str, level: i64) -> Option<&'a Value> {
    lists.pointer(&format!("/{list_id}/listProperties/nestingLevels/{level}"))
}

fn list_is_ordered(lists: &Value, list_id: &str, level: i64) -> bool {
    list_level(lists, list_id, level)
        .and_then(|value| value.get("glyphType"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty() && value != "GLYPH_TYPE_UNSPECIFIED")
}

fn list_is_checklist(lists: &Value, list_id: &str, level: i64) -> bool {
    list_level(lists, list_id, level).is_some_and(|value| {
        let glyph = strv(value, "glyphType", "");
        matches!(glyph, "" | "GLYPH_TYPE_UNSPECIFIED") && value.get("glyphSymbol").is_none()
    })
}

fn paragraph_is_checked(paragraph: &Value) -> bool {
    arr(paragraph, "elements").iter().any(|element| {
        element.get("textRun").is_some_and(|run| {
            !strv(run, "content", "").trim().is_empty()
                && run
                    .pointer("/textStyle/strikethrough")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
    })
}

fn markdown_content(content: &[Value], doc: &Value) -> String {
    content
        .iter()
        .filter_map(|element| {
            element
                .get("paragraph")
                .map(|paragraph| paragraph_markdown(paragraph, doc, false))
                .or_else(|| {
                    element.get("table").map(|table| {
                        arr(table, "tableRows")
                            .iter()
                            .flat_map(|row| arr(row, "tableCells"))
                            .map(|cell| table_cell_markdown(cell, doc))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                })
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn table_cell_markdown(cell: &Value, doc: &Value) -> String {
    markdown_content(arr(cell, "content"), doc)
}

fn paragraph_markdown(p: &Value, doc: &Value, skip_strikethrough: bool) -> String {
    let mut out = String::new();
    for e in arr(p, "elements") {
        if let Some(run) = e.get("textRun") {
            let mut text = strv(run, "content", "")
                .trim_end_matches('\n')
                .replace('\u{e907}', "[Smart Chip]");
            if text.is_empty() {
                continue;
            }
            let style = run.get("textStyle").unwrap_or(&Value::Null);
            let mono = style
                .pointer("/weightedFontFamily/fontFamily")
                .and_then(Value::as_str)
                .is_some_and(|f| {
                    matches!(
                        f,
                        "Courier New" | "Consolas" | "Roboto Mono" | "Source Code Pro"
                    )
                });
            if mono {
                text = format!("`{text}`");
            } else {
                let b = style.get("bold").and_then(Value::as_bool).unwrap_or(false);
                let i = style
                    .get("italic")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                text = match (b, i) {
                    (true, true) => format!("***{text}***"),
                    (true, false) => format!("**{text}**"),
                    (false, true) => format!("*{text}*"),
                    _ => text,
                };
                if !skip_strikethrough
                    && style
                        .get("strikethrough")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    text = format!("~~{text}~~");
                }
            }
            if let Some(url) = style.pointer("/link/url").and_then(Value::as_str) {
                text = format!("[{text}]({url})");
            }
            out.push_str(&text);
        } else if let Some(person) = e.pointer("/person/personProperties") {
            let name = strv(person, "name", "");
            let email = strv(person, "email", "");
            if !email.is_empty() {
                out.push_str(&format!(
                    "[{}](mailto:{email})",
                    if name.is_empty() { email } else { name }
                ));
            }
        } else if let Some(properties) = e.pointer("/richLink/richLinkProperties") {
            let title = strv(properties, "title", "");
            let uri = strv(properties, "uri", "");
            if !title.is_empty() && !uri.is_empty() {
                out.push_str(&format!("[{title}]({uri})"));
            } else {
                out.push_str(if uri.is_empty() { title } else { uri });
            }
        } else if let Some(properties) = e.pointer("/dateElement/dateElementProperties") {
            let display = strv(properties, "displayText", "");
            out.push_str(if display.is_empty() {
                strv(properties, "timestamp", "")
            } else {
                display
            });
        } else if let Some(footnote_id) = e
            .pointer("/footnoteReference/footnoteId")
            .and_then(Value::as_str)
        {
            out.push_str(&format!("[^{footnote_id}]"));
        } else if let Some(auto_type) = e.pointer("/autoText/type").and_then(Value::as_str) {
            match auto_type {
                "PAGE_NUMBER" => out.push_str("[Page #]"),
                "PAGE_COUNT" => out.push_str("[Page Count]"),
                _ => {}
            }
        } else if let Some(obj) = e
            .pointer("/inlineObjectElement/inlineObjectId")
            .and_then(Value::as_str)
        {
            if let Some(props) = doc.pointer(&format!(
                "/inlineObjects/{obj}/inlineObjectProperties/embeddedObject"
            )) {
                let title = strv(props, "title", strv(props, "description", ""));
                if let Some(uri) = props
                    .pointer("/imageProperties/contentUri")
                    .and_then(Value::as_str)
                {
                    out.push_str(&format!("![{title}]({uri})"));
                } else {
                    out.push_str("[Image]");
                }
            }
        } else if e.get("horizontalRule").is_some() {
            out.push_str("\n---\n");
        } else if e.get("equation").is_some() {
            out.push_str("[Equation]");
        }
    }
    out.trim().to_owned()
}

#[derive(Clone)]
struct RawComment {
    author: String,
    content: String,
    anchor: String,
    replies: Vec<(String, String)>,
    resolved: bool,
}
async fn fetch_raw_comments(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    include_resolved: bool,
) -> Result<Vec<RawComment>, String> {
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut q=vec![("fields".to_owned(),"comments(id,content,author,createdTime,modifiedTime,resolved,quotedFileContent,replies(id,content,author,createdTime,modifiedTime)),nextPageToken".to_owned()),("includeDeleted".into(),"false".into()),("pageSize".into(),"100".into())];
        if let Some(t) = &token {
            q.push(("pageToken".into(), t.clone()));
        }
        let v = session
            .send_json(
                tool,
                session
                    .request(Method::GET, &format!("{DRIVE_API}/{}/comments", path(id)))
                    .query(&q),
            )
            .await?;
        for c in arr(&v, "comments") {
            let resolved = c.get("resolved").and_then(Value::as_bool).unwrap_or(false);
            if resolved && !include_resolved {
                continue;
            }
            out.push(RawComment {
                author: c
                    .pointer("/author/displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown")
                    .into(),
                content: strv(c, "content", "").into(),
                anchor: c
                    .pointer("/quotedFileContent/value")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                replies: arr(c, "replies")
                    .iter()
                    .map(|r| {
                        (
                            r.pointer("/author/displayName")
                                .and_then(Value::as_str)
                                .unwrap_or("Unknown")
                                .into(),
                            strv(r, "content", "").into(),
                        )
                    })
                    .collect(),
                resolved,
            });
        }
        token = v
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if token.is_none() {
            break;
        }
    }
    Ok(out)
}
fn format_comments_inline(mut md: String, comments: &[RawComment]) -> String {
    let mut notes = Vec::new();
    let mut unmatched = Vec::new();
    for (i, c) in comments.iter().enumerate() {
        let n = i + 1;
        if !c.anchor.is_empty() && md.contains(&c.anchor) {
            md = md.replacen(&c.anchor, &format!("{}[^c{n}]", c.anchor), 1);
            let mut note = format!("[^c{n}]: **{}**: {}", c.author, c.content);
            for (a, t) in &c.replies {
                note.push_str(&format!("\n    - **{a}**: {t}"));
            }
            notes.push(note);
        } else {
            unmatched.push(c.clone());
        }
    }
    if !notes.is_empty() {
        md = format!("{}\n\n{}\n", md.trim_end(), notes.join("\n"));
    }
    if !unmatched.is_empty() {
        md = format!(
            "{}\n\n{}",
            md.trim_end(),
            format_comments_appendix(&unmatched)
        );
    }
    md
}
fn format_comments_appendix(comments: &[RawComment]) -> String {
    let mut lines = vec!["## Comments".into(), String::new()];
    for c in comments {
        if !c.anchor.is_empty() {
            lines.push(format!("> {}", c.anchor));
            lines.push(String::new());
        }
        lines.push(format!(
            "- **{}**: {}{}",
            c.author,
            c.content,
            if c.resolved { " *(Resolved)*" } else { "" }
        ));
        for (a, t) in &c.replies {
            lines.push(format!("  - **{a}**: {t}"));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

#[derive(Clone)]
struct Cell {
    start: i64,
    end: i64,
    insert: i64,
    text: String,
    elements: usize,
}
#[derive(Clone)]
struct TableInfo {
    start: i64,
    end: i64,
    rows: usize,
    columns: usize,
    cells: Vec<Vec<Cell>>,
    data: Vec<Vec<String>>,
}
fn find_tables_in(content: &[Value]) -> Vec<TableInfo> {
    let mut out = Vec::new();
    for e in content {
        if let Some(t) = e.get("table") {
            let mut cells = Vec::new();
            let mut data = Vec::new();
            for row in arr(t, "tableRows") {
                let mut cr = Vec::new();
                let mut dr = Vec::new();
                for cell in arr(row, "tableCells") {
                    let start = cell.get("startIndex").and_then(Value::as_i64).unwrap_or(0);
                    let end = cell
                        .get("endIndex")
                        .and_then(Value::as_i64)
                        .unwrap_or(start);
                    let mut text = String::new();
                    extract_elements_text(arr(cell, "content"), &mut text, 0);
                    let insert = arr(cell, "content")
                        .iter()
                        .filter_map(|element| element.get("paragraph"))
                        .flat_map(|paragraph| arr(paragraph, "elements").iter())
                        .find_map(|element| element.get("startIndex").and_then(Value::as_i64))
                        .unwrap_or(start + 1);
                    cr.push(Cell {
                        start,
                        end,
                        insert,
                        text: text.trim().into(),
                        elements: arr(cell, "content").len(),
                    });
                    dr.push(text.trim().into());
                }
                cells.push(cr);
                data.push(dr);
            }
            out.push(TableInfo {
                start: e.get("startIndex").and_then(Value::as_i64).unwrap_or(0),
                end: e.get("endIndex").and_then(Value::as_i64).unwrap_or(0),
                rows: cells.len(),
                columns: cells.first().map_or(0, Vec::len),
                cells,
                data,
            });
        }
    }
    out
}
fn validate_table_data(rows: &[Value]) -> Result<(), String> {
    if rows.is_empty() || rows.len() > 1000 {
        return Err("table_data must contain 1-1000 rows".into());
    }
    let cols = rows[0].as_array().map_or(0, Vec::len);
    if cols == 0 || cols > 20 {
        return Err("table_data must contain 1-20 columns".into());
    }
    if rows.iter().any(|r| {
        r.as_array()
            .is_none_or(|c| c.len() != cols || c.iter().any(|v| !v.is_string()))
    }) {
        return Err("table_data must be a rectangular 2D list of strings".into());
    }
    Ok(())
}
fn validate_table_dims(r: i64, c: i64) -> Result<(), String> {
    if !(1..=1000).contains(&r) || !(1..=20).contains(&c) {
        Err("table dimensions must be 1-1000 rows and 1-20 columns".into())
    } else {
        Ok(())
    }
}

fn body_content(v: &Value) -> &[Value] {
    v.pointer("/body/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn find_tab<'a>(tabs: &'a [Value], id: &str) -> Option<&'a Value> {
    for t in tabs {
        if t.pointer("/tabProperties/tabId").and_then(Value::as_str) == Some(id) {
            return Some(t);
        }
        if let Some(found) = find_tab(arr(t, "childTabs"), id) {
            return Some(found);
        }
    }
    None
}
fn first_document_tab(tabs: &[Value]) -> Option<&Value> {
    for t in tabs {
        if let Some(d) = t.get("documentTab") {
            return Some(d);
        }
        if let Some(d) = first_document_tab(arr(t, "childTabs")) {
            return Some(d);
        }
    }
    None
}
fn tab_summaries(tabs: &[Value]) -> Vec<Value> {
    tabs.iter().map(|t|{let mut v=json!({"title":t.pointer("/tabProperties/title"),"tab_id":t.pointer("/tabProperties/tabId")});let child=tab_summaries(arr(t,"childTabs"));if !child.is_empty(){v["child_tabs"]=Value::Array(child);}v}).collect()
}
fn element_summary(e: &Value) -> Value {
    let typ = if e.get("table").is_some() {
        "table"
    } else if e.get("paragraph").is_some() {
        "paragraph"
    } else if e.get("sectionBreak").is_some() {
        "section_break"
    } else {
        "unknown"
    };
    let mut v = json!({"type":typ,"start_index":e.get("startIndex"),"end_index":e.get("endIndex")});
    if typ == "paragraph" {
        let mut text = String::new();
        extract_elements_text(std::slice::from_ref(e), &mut text, 0);
        v["text_preview"] = json!(text.chars().take(100).collect::<String>());
    }
    if typ == "table" {
        let t = find_tables_in(std::slice::from_ref(e));
        if let Some(t) = t.first() {
            v["rows"] = json!(t.rows);
            v["columns"] = json!(t.columns);
            v["cell_count"] = json!(t.rows * t.columns);
        }
    }
    v
}
fn add_segments(result: &mut Map<String, Value>, doc: &Value, key: &str, kind: &str) {
    let mut entries = Vec::new();
    if let Some(map) = doc.get(key).and_then(Value::as_object) {
        for (id, seg) in map {
            let content = arr(seg, "content");
            let mut text = String::new();
            extract_elements_text(content, &mut text, 0);
            entries.push(json!({"segment_id":id,"start_index":content.first().and_then(|e|e.get("startIndex")),"end_index":content.last().and_then(|e|e.get("endIndex")),"content_preview":text.chars().take(100).collect::<String>(),"element_count":content.len(),"source":"segment_content","type":kind}));
        }
    }
    if !entries.is_empty() {
        result.insert(key.into(), Value::Array(entries));
    }
}
fn style_segment_key(section: &str, typ: &str) -> &'static str {
    match (section, typ) {
        ("header", "FIRST_PAGE_ONLY") => "firstPageHeaderId",
        ("header", "EVEN_PAGE") => "evenPageHeaderId",
        ("header", _) => "defaultHeaderId",
        ("footer", "FIRST_PAGE_ONLY") => "firstPageFooterId",
        ("footer", "EVEN_PAGE") => "evenPageFooterId",
        _ => "defaultFooterId",
    }
}
fn find_style_segment_id(doc: &Value, key: &str) -> Option<String> {
    doc.pointer(&format!("/documentStyle/{key}"))
        .and_then(Value::as_str)
        .or_else(|| {
            first_document_tab(arr(doc, "tabs")).and_then(|d| {
                d.pointer(&format!("/documentStyle/{key}"))
                    .and_then(Value::as_str)
            })
        })
        .map(str::to_owned)
}
fn segment_bounds(doc: &Value, section: &str, id: &str) -> Option<(i64, i64)> {
    let plural = if section == "header" {
        "headers"
    } else {
        "footers"
    };
    let seg = doc.pointer(&format!("/{plural}/{id}")).or_else(|| {
        first_document_tab(arr(doc, "tabs")).and_then(|d| d.pointer(&format!("/{plural}/{id}")))
    })?;
    let content = arr(seg, "content");
    Some((
        content.first()?.get("startIndex")?.as_i64()?,
        content.last()?.get("endIndex")?.as_i64()?,
    ))
}

async fn authenticated_download(
    session: &GoogleSession,
    tool: &str,
    url: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut response = session
        .request(Method::GET, url)
        .send()
        .await
        .map_err(|_| format!("API error in {tool}: Google API request failed"))?;
    if !response.status().is_success() {
        return Err(format!(
            "API error in {tool}: Google API returned {}",
            response.status()
        ));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(format!("Content exceeds the {limit} byte limit"));
    }
    let mut out = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| format!("API error in {tool}: failed to read response"))?
    {
        if out.len().saturating_add(chunk.len()) > limit {
            return Err(format!("Content exceeds the {limit} byte limit"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
fn validate_suggestion_mode(mode: &str) -> Result<(), String> {
    if [
        "DEFAULT_FOR_CURRENT_ACCESS",
        "SUGGESTIONS_INLINE",
        "PREVIEW_SUGGESTIONS_ACCEPTED",
        "PREVIEW_WITHOUT_SUGGESTIONS",
    ]
    .contains(&mode)
    {
        Ok(())
    } else {
        Err(format!(
            "Error: suggestions_view_mode must be one of DEFAULT_FOR_CURRENT_ACCESS, SUGGESTIONS_INLINE, PREVIEW_SUGGESTIONS_ACCEPTED, PREVIEW_WITHOUT_SUGGESTIONS, got '{mode}'"
        ))
    }
}
fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 255
        || name.contains(['/', '\\', '\0'])
        || name == "."
        || name == ".."
    {
        Err("Invalid PDF filename".into())
    } else {
        Ok(())
    }
}

fn req_doc_id(a: &Map<String, Value>) -> Result<String, String> {
    let id = extract_document_id(&req_str(a, "document_id")?);
    validate_document_id(&id)?;
    Ok(id)
}
fn req_str(a: &Map<String, Value>, name: &str) -> Result<String, String> {
    map_req_str(a, name)
}
fn opt_str(a: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    map_opt_str(a, name)
}
fn req_i64(a: &Map<String, Value>, name: &str) -> Result<i64, String> {
    map_req_i64(a, name)
}
fn opt_i64(a: &Map<String, Value>, name: &str) -> Result<Option<i64>, String> {
    map_opt_i64(a, name)
}
fn int_or(a: &Map<String, Value>, name: &str, default: i64) -> Result<i64, String> {
    Ok(opt_i64(a, name)?.unwrap_or(default))
}
fn bool_or(a: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    map_bool_or(a, name, default)
}
fn string_or(a: &Map<String, Value>, name: &str, default: &str) -> Result<String, String> {
    map_string_or(a, name, default)
}
fn map_req_str(a: &Map<String, Value>, name: &str) -> Result<String, String> {
    a.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing required argument: {name}"))
}
fn map_opt_str(a: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    match a.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        _ => Err(format!("{name} must be a string or null")),
    }
}
fn map_string_or(a: &Map<String, Value>, name: &str, default: &str) -> Result<String, String> {
    Ok(map_opt_str(a, name)?.unwrap_or_else(|| default.into()))
}
fn map_req_i64(a: &Map<String, Value>, name: &str) -> Result<i64, String> {
    a.get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Missing required argument: {name}"))
}
fn map_opt_i64(a: &Map<String, Value>, name: &str) -> Result<Option<i64>, String> {
    match a.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer or null")),
    }
}
fn map_opt_f64(a: &Map<String, Value>, name: &str) -> Result<Option<f64>, String> {
    match a.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a number or null")),
    }
}
fn map_opt_bool(a: &Map<String, Value>, name: &str) -> Result<Option<bool>, String> {
    match a.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a boolean or null")),
    }
}
fn map_bool_or(a: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    Ok(map_opt_bool(a, name)?.unwrap_or(default))
}
fn map_string_array(a: &Map<String, Value>, name: &str) -> Result<Option<Vec<String>>, String> {
    match a.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_array()
            .filter(|x| x.iter().all(Value::is_string))
            .map(|x| {
                x.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .map(Some)
            .ok_or_else(|| format!("{name} must be an array of strings")),
    }
}
fn map_i64_array(a: &Map<String, Value>, name: &str) -> Result<Option<Vec<i64>>, String> {
    match a.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_array()
            .filter(|items| {
                items.len() <= MAX_EXPANDING_INDICES
                    && items.iter().all(|item| item.as_i64().is_some())
            })
            .map(|items| items.iter().filter_map(Value::as_i64).collect())
            .map(Some)
            .ok_or_else(|| {
                format!("{name} must be an array of at most {MAX_EXPANDING_INDICES} integers")
            }),
    }
}
fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn strv<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
fn display(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "None".into(),
        Some(v) => v.to_string(),
    }
}
fn nonzero(v: i64) -> Option<i64> {
    (v != 0).then_some(v)
}
