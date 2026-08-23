use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use reqwest::{Method, StatusCode, Url};
use serde_json::{Map, Value, json};

use crate::auth::GoogleSession;

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const READ: &str = "https://www.googleapis.com/auth/gmail.readonly";
const SEND: &str = "https://www.googleapis.com/auth/gmail.send";
const COMPOSE: &str = "https://www.googleapis.com/auth/gmail.compose";
const MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";
const LABELS: &str = "https://www.googleapis.com/auth/gmail.labels";
const SETTINGS: &str = "https://www.googleapis.com/auth/gmail.settings.basic";
const READ_SCOPES: &[&[&str]] = &[&[READ], &[MODIFY]];
const SEND_SCOPES: &[&[&str]] = &[&[READ, SEND], &[MODIFY]];
const COMPOSE_SCOPES: &[&[&str]] = &[&[COMPOSE], &[MODIFY]];
const MODIFY_SCOPES: &[&[&str]] = &[&[MODIFY]];
const LABEL_SCOPES: &[&[&str]] = &[&[LABELS], &[MODIFY]];
const SETTINGS_SCOPES: &[&[&str]] = &[&[SETTINGS]];
const MAX_ATTACHMENT: usize = 25 * 1024 * 1024;
const MAX_BODY: usize = 20_000;
const CHUNK: usize = 25;
const HEADER_CHUNK: usize = 10;
const EXPIRY: u64 = 3600;
const HEADERS: &[&str] = &[
    "Subject",
    "From",
    "Reply-To",
    "To",
    "Cc",
    "Message-ID",
    "In-Reply-To",
    "References",
    "Date",
    "List-Unsubscribe",
    "Precedence",
    "List-Id",
];
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub async fn dispatch(name: &str, a: &Map<String, Value>) -> Result<String, String> {
    match name {
        "search_gmail_messages" => search(a).await,
        "get_gmail_message_content" => get_message(a).await,
        "get_gmail_messages_content_batch" => get_messages(a).await,
        "send_gmail_message" => send_message(a).await,
        "get_gmail_attachment_content" => get_attachment(a).await,
        "get_gmail_thread_content" => get_thread(a).await,
        "modify_gmail_message_labels" => modify_labels(a).await,
        "list_gmail_labels" => list_labels(a).await,
        "manage_gmail_label" => manage_label(a).await,
        "draft_gmail_message" => draft_message(a).await,
        "list_gmail_filters" => list_filters(a).await,
        "manage_gmail_filter" => manage_filter(a).await,
        "get_gmail_threads_content_batch" => get_threads(a).await,
        "batch_modify_gmail_message_labels" => batch_modify_labels(a).await,
        _ => Err(format!("Unsupported native Gmail tool: {name}")),
    }
}

const SEARCH_ARGS: &[&str] = &[
    "user_google_email",
    "query",
    "page_size",
    "page_token",
    "include_headers",
];
const GET_ARGS: &[&str] = &["user_google_email", "message_id", "body_format", "full"];
const GETS_ARGS: &[&str] = &["user_google_email", "message_ids", "format", "body_format"];
const ATT_ARGS: &[&str] = &[
    "user_google_email",
    "message_id",
    "attachment_id",
    "return_base64",
];
const SEND_ARGS: &[&str] = &[
    "user_google_email",
    "to",
    "subject",
    "body",
    "body_format",
    "forward_message_id",
    "include_forwarded_attachments",
    "cc",
    "bcc",
    "from_name",
    "from_email",
    "thread_id",
    "in_reply_to",
    "references",
    "attachments",
    "include_signature",
    "quote_original",
    "reply_all",
];
const DRAFT_ARGS: &[&str] = &[
    "user_google_email",
    "subject",
    "body",
    "body_format",
    "to",
    "cc",
    "bcc",
    "from_name",
    "from_email",
    "thread_id",
    "in_reply_to",
    "references",
    "attachments",
    "include_signature",
    "quote_original",
];
const THREAD_ARGS: &[&str] = &[
    "user_google_email",
    "thread_id",
    "body_format",
    "include_analysis",
];
const THREADS_ARGS: &[&str] = &["user_google_email", "thread_ids", "body_format"];
const USER_ARGS: &[&str] = &["user_google_email"];
const LABEL_ARGS: &[&str] = &[
    "user_google_email",
    "action",
    "name",
    "label_id",
    "label_list_visibility",
    "message_list_visibility",
];
const FILTER_ARGS: &[&str] = &[
    "user_google_email",
    "action",
    "criteria",
    "filter_action",
    "filter_id",
];
const MODIFY_ARGS: &[&str] = &[
    "user_google_email",
    "message_id",
    "add_label_ids",
    "remove_label_ids",
];
const BATCH_MODIFY_ARGS: &[&str] = &[
    "user_google_email",
    "message_ids",
    "add_label_ids",
    "remove_label_ids",
];

async fn search(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SEARCH_ARGS)?;
    let q = req_str(a, "query")?;
    let size = int(a, "page_size", 10)?;
    if !(1..=500).contains(&size) {
        return Err("page_size must be between 1 and 500".into());
    }
    let session = auth(a, READ_SCOPES).await?;
    let mut query = vec![
        ("q".into(), q.clone()),
        ("maxResults".into(), size.to_string()),
    ];
    if let Some(v) = opt_str(a, "page_token")?.filter(|v| !v.is_empty()) {
        query.push(("pageToken".into(), v));
    }
    let response = api(
        &session,
        "search_gmail_messages",
        Method::GET,
        &format!("{API}/messages"),
        &query,
        None,
        Retry::Safe,
    )
    .await?;
    let messages = arr(&response, "messages");
    if messages.is_empty() {
        return Ok(format!("No messages found for query: '{q}'"));
    }
    let include = bool_arg(a, "include_headers", false)?;
    let mut metadata = HashMap::new();
    if include {
        let ids: Vec<_> = messages
            .iter()
            .filter_map(|v| s(v, "id").map(str::to_owned))
            .collect();
        for chunk in ids.chunks(HEADER_CHUNK) {
            for id in chunk {
                metadata.insert(
                    id.clone(),
                    fetch_message(&session, id, "metadata")
                        .await
                        .ok()
                        .map(|v| headers(v.get("payload").unwrap_or(&Value::Null))),
                );
            }
        }
    }
    let mut out = vec![
        format!("Found {} messages matching '{q}':", messages.len()),
        "".into(),
        "📧 MESSAGES:".into(),
    ];
    for (i, m) in messages.iter().enumerate() {
        let id = s(m, "id").unwrap_or("unknown");
        let tid = s(m, "threadId").unwrap_or("unknown");
        out.push(format!("  {}. Message ID: {id}", i + 1));
        if include {
            match metadata.get(id).and_then(Option::as_ref) {
                Some(h) => out.extend([
                    format!("     Subject: {}", hv(h, "Subject", "(no subject)")),
                    format!("     From: {}", hv(h, "From", "(unknown sender)")),
                    format!("     Date: {}", hv(h, "Date", "(unknown date)")),
                ]),
                None => out.push("     Headers: unavailable (metadata fetch failed)".into()),
            }
        }
        out.extend([
            format!("     Web Link: {}", web(id)),
            format!("     Thread ID: {tid}"),
            format!("     Thread Link: {}", web(tid)),
            "".into(),
        ]);
    }
    out.extend([
        "💡 USAGE:".into(),
        "  • Pass Message IDs as a list to get_gmail_messages_content_batch()".into(),
        "  • Pass Thread IDs to get_gmail_thread_content() or get_gmail_threads_content_batch()"
            .into(),
    ]);
    if let Some(token) = s(&response, "nextPageToken") {
        out.extend(["".into(), format!("📄 PAGINATION: To get the next page, call search_gmail_messages again with page_token='{token}'")]);
    }
    Ok(out.join("\n"))
}

async fn get_message(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, GET_ARGS)?;
    let id = req_str(a, "message_id")?;
    let fmt = enum_arg(a, "body_format", "text", &["text", "html", "raw"])?;
    let full = bool_arg(a, "full", false)?;
    let session = auth(a, READ_SCOPES).await?;
    let metadata = fetch_message(&session, &id, "metadata").await?;
    let h = headers(metadata.get("payload").unwrap_or(&Value::Null));
    if full {
        let (bytes, ext, mime, note) = if fmt == "raw" {
            let v = fetch_message(&session, &id, "raw").await?;
            (
                b64decode(s(&v, "raw").unwrap_or(""), true, usize::MAX)?,
                ".eml",
                "message/rfc822",
                None,
            )
        } else {
            let v = fetch_message(&session, &id, "full").await?;
            let (plain, html) = bodies(v.get("payload").unwrap_or(&Value::Null));
            if fmt == "html" && !html.trim().is_empty() {
                (html.into_bytes(), ".html", "text/html", None)
            } else if fmt == "html" {
                (
                    plain.into_bytes(),
                    ".txt",
                    "text/plain",
                    Some("No HTML body present; exported the plaintext body instead."),
                )
            } else if !plain.trim().is_empty() {
                (plain.into_bytes(), ".txt", "text/plain", None)
            } else {
                (html_text(&html).into_bytes(), ".txt", "text/plain", None)
            }
        };
        if bytes.is_empty() {
            return Err("message has no readable body content to export".into());
        }
        return export(&session, &h, bytes, ext, mime, note).await;
    }
    if fmt == "raw" {
        let v = fetch_message(&session, &id, "raw").await?;
        let bytes = b64decode(s(&v, "raw").unwrap_or(""), true, usize::MAX)?;
        let mut out = header_lines(&h, None);
        out.push(format!(
            "\n--- RAW MIME ---\n{}",
            truncate(&String::from_utf8_lossy(&bytes))
        ));
        return Ok(out.join("\n"));
    }
    let v = fetch_message(&session, &id, "full").await?;
    Ok(format_message(&v, &id, &fmt, None, false))
}

async fn get_messages(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, GETS_ARGS)?;
    let ids = str_list(a, "message_ids", false)?.unwrap_or_default();
    if ids.is_empty() {
        return Err("No message IDs provided".into());
    }
    let format = enum_arg(a, "format", "full", &["full", "metadata"])?;
    let body_fmt = enum_arg(a, "body_format", "text", &["text", "html", "raw"])?;
    if format == "metadata" && body_fmt != "text" {
        return Err("body_format='html' and body_format='raw' require format='full'.".into());
    }
    let session = auth(a, READ_SCOPES).await?;
    let mut out = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        for id in chunk {
            let fetch_fmt = if format == "metadata" || body_fmt == "raw" {
                "metadata"
            } else {
                "full"
            };
            match fetch_message(&session, id, fetch_fmt).await {
                Err(e) => out.push(format!("⚠️ Message {id}: {e}\n")),
                Ok(v) if format == "metadata" => {
                    out.push(format_message(&v, id, "text", None, true))
                }
                Ok(v) if body_fmt == "raw" => match fetch_message(&session, id, "raw").await {
                    Ok(raw) => {
                        let decoded = b64decode(s(&raw, "raw").unwrap_or(""), true, usize::MAX)
                            .map(|b| truncate(&String::from_utf8_lossy(&b)))
                            .unwrap_or_else(|e| format!("[Failed to decode raw MIME: {e}]"));
                        out.push(format_message(&v, id, "raw", Some(&decoded), false));
                    }
                    Err(e) => out.push(format!("⚠️ Message {id}: {e}\n")),
                },
                Ok(v) => out.push(format_message(&v, id, &body_fmt, None, false)),
            }
        }
    }
    Ok(format!(
        "Retrieved {} messages:\n\n{}",
        ids.len(),
        out.join("\n---\n\n")
    ))
}

async fn get_attachment(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, ATT_ARGS)?;
    let mid = req_str(a, "message_id")?;
    let aid = req_str(a, "attachment_id")?;
    let return_b64 = bool_arg(a, "return_base64", false)?;
    let session = auth(a, READ_SCOPES).await?;
    let url = format!(
        "{API}/messages/{}/attachments/{}",
        path_id(&mid)?,
        path_id(&aid)?
    );
    let value = match api(
        &session,
        "get_gmail_attachment_content",
        Method::GET,
        &url,
        &[],
        None,
        Retry::Safe,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            return Ok(format!(
                "Error: Failed to download attachment. The attachment ID may have changed.\nPlease fetch the message content again to get an updated attachment ID.\n\nError details: {e}"
            ));
        }
    };
    let encoded = s(&value, "data").unwrap_or("");
    let bytes = b64decode(encoded, true, MAX_ATTACHMENT)?;
    let size = value
        .get("size")
        .and_then(Value::as_u64)
        .unwrap_or(bytes.len() as u64);
    let (filename, _mime) = attachment_meta(&session, &mid, &aid, size)
        .await
        .unwrap_or(("attachment".into(), "application/octet-stream".into()));
    let mut out = vec![
        "Attachment downloaded successfully!".into(),
        format!("Message ID: {mid}"),
        format!("Filename: {filename}"),
        format!("Size: {:.1} KB ({size} bytes)", size as f64 / 1024.0),
    ];
    if stateless() {
        out.extend([
            "\n⚠️ Stateless mode: File storage disabled.".into(),
            "\nBase64-encoded content (first 100 characters shown):".into(),
            format!("{}...", encoded.chars().take(100).collect::<String>()),
        ]);
    } else {
        match save(&session.user_email, &filename, &bytes) {
            Ok(saved) => {
                out.push(format!("Saved filename: {}", saved.name));
                if transport() == "stdio" {
                    out.push(format!("\n📎 Saved to: {}", saved.path.display()));
                } else {
                    out.extend([
                        format!("\n📎 Download URL: {}", attachment_url(&saved.id)?),
                        "\nThe file will expire after 1 hour.".into(),
                    ]);
                }
            }
            Err(e) => out.push(format!(
                "\n⚠️ Failed to save attachment file. Showing preview instead.\nError: {e}"
            )),
        }
    }
    out.push(
        "\nNote: Attachment IDs are ephemeral. Always use IDs from the most recent message fetch."
            .into(),
    );
    if return_b64 {
        let standard = b64encode(&bytes, false);
        out.extend([
            format!(
                "\n📦 Base64 content ({} chars, standard base64):",
                standard.len()
            ),
            standard,
        ]);
    }
    Ok(out.join("\n"))
}

async fn send_message(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SEND_ARGS)?;
    let session = auth(a, SEND_SCOPES).await?;
    let mut c = Compose::from_args(a, &session.user_email, false)?;
    if let Some(mid) = opt_str(a, "forward_message_id")?.filter(|v| !v.is_empty()) {
        forward(
            &session,
            &mut c,
            &mid,
            bool_arg(a, "include_forwarded_attachments", true)?,
        )
        .await?;
    } else {
        if c.subject.is_none() || c.body.is_none() {
            return Err("Both 'subject' and 'body' are required when sending a message (they are optional only when forwarding via 'forward_message_id').".into());
        }
        if c.reply_all && c.thread_id.is_none() {
            return Err("'reply_all' requires a thread_id".into());
        }
        if c.to.is_none() && !(c.reply_all && c.thread_id.is_some()) {
            return Err(
                "'to' is required unless replying with reply_all=True and a thread_id".into(),
            );
        }
    }
    enrich(&session, &mut c, true).await?;
    c.resolve().await?;
    let requested = c.requested;
    let attached = c.attachments.len();
    if requested > 0 && attached == 0 {
        return Err(
            "No valid attachments were added. Verify each attachment path/content and retry."
                .into(),
        );
    }
    let raw = mime_message(&c)?;
    let mut body = json!({"raw": b64encode(raw.as_bytes(), true)});
    if let Some(tid) = &c.thread_id {
        body["threadId"] = Value::String(tid.clone());
    }
    // Never retry this non-idempotent POST: a failed response may follow a successful send.
    let sent = api(
        &session,
        "send_gmail_message",
        Method::POST,
        &format!("{API}/messages/send"),
        &[],
        Some(&body),
        Retry::Never,
    )
    .await?;
    Ok(format!(
        "Email sent{}! Message ID: {}",
        attachment_result(attached, requested),
        s(&sent, "id").unwrap_or("unknown")
    ))
}

async fn draft_message(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, DRAFT_ARGS)?;
    let session = auth(a, COMPOSE_SCOPES).await?;
    let mut c = Compose::from_args(a, &session.user_email, true)?;
    enrich(&session, &mut c, false).await?;
    c.resolve().await?;
    let requested = c.requested;
    let attached = c.attachments.len();
    if requested > 0 && attached == 0 {
        return Err(
            "No valid attachments were added. Verify each attachment path/content and retry."
                .into(),
        );
    }
    let mut message = json!({"raw": b64encode(mime_message(&c)?.as_bytes(), true)});
    if c.thread_id.is_some() && c.in_reply_to.is_some() && c.references.is_some() {
        message["threadId"] = Value::String(c.thread_id.clone().unwrap_or_default());
    }
    let v = api(
        &session,
        "draft_gmail_message",
        Method::POST,
        &format!("{API}/drafts"),
        &[],
        Some(&json!({"message": message})),
        Retry::Never,
    )
    .await?;
    Ok(format!(
        "Draft created{}! Draft ID: {}",
        attachment_result(attached, requested),
        s(&v, "id").unwrap_or("unknown")
    ))
}

async fn get_thread(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, THREAD_ARGS)?;
    let id = req_str(a, "thread_id")?;
    let fmt = enum_arg(a, "body_format", "text", &["text", "html", "raw"])?;
    let analysis = bool_arg(a, "include_analysis", false)?;
    let session = auth(a, READ_SCOPES).await?;
    let thread = fetch_thread(&session, &id).await?;
    let content = format_thread(&session, &thread, &id, &fmt).await;
    if analysis {
        serde_json::to_string_pretty(
            &json!({"content": content, "analysis": analyze(&thread, &session.user_email)}),
        )
        .map_err(|e| e.to_string())
    } else {
        Ok(content)
    }
}

async fn get_threads(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, THREADS_ARGS)?;
    let ids = str_list(a, "thread_ids", false)?.unwrap_or_default();
    if ids.is_empty() {
        return Err("No thread IDs provided".into());
    }
    let fmt = enum_arg(a, "body_format", "text", &["text", "html", "raw"])?;
    let session = auth(a, READ_SCOPES).await?;
    let mut out = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        for id in chunk {
            match fetch_thread(&session, id).await {
                Ok(t) => out.push(format_thread(&session, &t, id, &fmt).await),
                Err(e) => out.push(format!("⚠️ Thread {id}: {e}\n")),
            }
        }
    }
    Ok(format!(
        "Retrieved {} threads:\n\n{}",
        ids.len(),
        out.join("\n---\n\n")
    ))
}

async fn list_labels(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, USER_ARGS)?;
    let session = auth(a, READ_SCOPES).await?;
    let v = api(
        &session,
        "list_gmail_labels",
        Method::GET,
        &format!("{API}/labels"),
        &[],
        None,
        Retry::Safe,
    )
    .await?;
    let labels = arr(&v, "labels");
    if labels.is_empty() {
        return Ok("No labels found.".into());
    }
    let mut sys = Vec::new();
    let mut user = Vec::new();
    for l in labels {
        let line = format!(
            "  • {} (ID: {})",
            s(l, "name").unwrap_or(""),
            s(l, "id").unwrap_or("")
        );
        if s(l, "type") == Some("system") {
            sys.push(line);
        } else {
            user.push(line);
        }
    }
    let mut out = vec![format!("Found {} labels:", labels.len()), "".into()];
    if !sys.is_empty() {
        out.push("📂 SYSTEM LABELS:".into());
        out.extend(sys);
        out.push("".into());
    }
    if !user.is_empty() {
        out.push("🏷️  USER LABELS:".into());
        out.extend(user);
    }
    Ok(out.join("\n"))
}

async fn manage_label(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, LABEL_ARGS)?;
    let action = enum_arg(a, "action", "", &["create", "update", "delete"])?;
    let name = opt_str(a, "name")?;
    let id = opt_str(a, "label_id")?;
    let lv = enum_arg(
        a,
        "label_list_visibility",
        "labelShow",
        &["labelShow", "labelHide"],
    )?;
    let mv = enum_arg(a, "message_list_visibility", "show", &["show", "hide"])?;
    let session = auth(a, LABEL_SCOPES).await?;
    match action.as_str() {
        "create" => {
            let name = name
                .filter(|v| !v.is_empty())
                .ok_or("Label name is required for create action.")?;
            no_ctl(&name, "name")?;
            let v = api(
                &session,
                "manage_gmail_label",
                Method::POST,
                &format!("{API}/labels"),
                &[],
                Some(&json!({"name":name,"labelListVisibility":lv,"messageListVisibility":mv})),
                Retry::Never,
            )
            .await?;
            Ok(format!(
                "Label created successfully!\nName: {}\nID: {}",
                s(&v, "name").unwrap_or(""),
                s(&v, "id").unwrap_or("")
            ))
        }
        "update" => {
            let id = id
                .filter(|v| !v.is_empty())
                .ok_or("Label ID is required for update action.")?;
            let url = format!("{API}/labels/{}", path_id(&id)?);
            let old = api(
                &session,
                "manage_gmail_label",
                Method::GET,
                &url,
                &[],
                None,
                Retry::Safe,
            )
            .await?;
            let name = name.unwrap_or_else(|| s(&old, "name").unwrap_or("").into());
            no_ctl(&name, "name")?;
            let v = api(&session,"manage_gmail_label",Method::PUT,&url,&[],Some(&json!({"id":id,"name":name,"labelListVisibility":lv,"messageListVisibility":mv})),Retry::Safe).await?;
            Ok(format!(
                "Label updated successfully!\nName: {}\nID: {}",
                s(&v, "name").unwrap_or(""),
                s(&v, "id").unwrap_or("")
            ))
        }
        "delete" => {
            let id = id
                .filter(|v| !v.is_empty())
                .ok_or("Label ID is required for delete action.")?;
            let url = format!("{API}/labels/{}", path_id(&id)?);
            let old = api(
                &session,
                "manage_gmail_label",
                Method::GET,
                &url,
                &[],
                None,
                Retry::Safe,
            )
            .await?;
            let name = s(&old, "name").unwrap_or("").to_owned();
            if let Err(error) = api(
                &session,
                "manage_gmail_label",
                Method::DELETE,
                &url,
                &[],
                None,
                Retry::Safe,
            )
            .await
                && !error.contains("404")
            {
                return Err(error);
            }
            Ok(format!("Label '{name}' (ID: {id}) deleted successfully!"))
        }
        _ => unreachable!(),
    }
}

async fn list_filters(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, USER_ARGS)?;
    let session = auth(a, SETTINGS_SCOPES).await?;
    let v = api(
        &session,
        "list_gmail_filters",
        Method::GET,
        &format!("{API}/settings/filters"),
        &[],
        None,
        Retry::Safe,
    )
    .await?;
    let filters = v
        .get("filter")
        .or_else(|| v.get("filters"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if filters.is_empty() {
        return Ok("No filters found.".into());
    }
    let mut out = vec![format!("Found {} filters:", filters.len()), "".into()];
    for f in filters {
        out.push(format!("🔹 Filter ID: {}", s(f, "id").unwrap_or("(no id)")));
        out.push("  Criteria:".into());
        let c = f.get("criteria").and_then(Value::as_object);
        let mut lines = Vec::new();
        for (k, l) in [
            ("from", "From"),
            ("to", "To"),
            ("subject", "Subject"),
            ("query", "Query"),
            ("negatedQuery", "Exclude Query"),
        ] {
            if let Some(x) = c.and_then(|o| o.get(k)).and_then(Value::as_str) {
                lines.push(format!("{l}: {x}"));
            }
        }
        if c.and_then(|o| o.get("hasAttachment"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            lines.push("Has attachment".into())
        }
        if c.and_then(|o| o.get("excludeChats"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            lines.push("Exclude chats".into())
        }
        if let Some(n) = c.and_then(|o| o.get("size")).and_then(Value::as_i64) {
            lines.push(format!(
                "Size {} {n} bytes",
                c.and_then(|o| o.get("sizeComparison"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ))
        }
        if lines.is_empty() {
            lines.push("(none)".into())
        }
        out.extend(lines.into_iter().map(|x| format!("    • {x}")));
        out.push("  Actions:".into());
        let ac = f.get("action").and_then(Value::as_object);
        let mut lines = Vec::new();
        if let Some(x) = ac.and_then(|o| o.get("forward")).and_then(Value::as_str) {
            lines.push(format!("Forward to: {x}"))
        }
        for (k, l) in [
            ("removeLabelIds", "Remove labels"),
            ("addLabelIds", "Add labels"),
        ] {
            if let Some(v) = ac.and_then(|o| o.get(k)).and_then(Value::as_array) {
                lines.push(format!(
                    "{l}: {}",
                    v.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
        if lines.is_empty() {
            lines.push("(none)".into())
        }
        out.extend(lines.into_iter().map(|x| format!("    • {x}")));
        out.push("".into());
    }
    Ok(out.join("\n").trim_end().to_owned())
}

async fn manage_filter(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, FILTER_ARGS)?;
    let action = req_str(a, "action")?.trim().to_ascii_lowercase();
    let criteria = dict(a, "criteria")?;
    let fa = dict(a, "filter_action")?;
    let id = opt_str(a, "filter_id")?;
    let session = auth(a, SETTINGS_SCOPES).await?;
    match action.as_str() {
        "create" => {
            let criteria = criteria
                .filter(|v| !v.is_empty())
                .ok_or("criteria and filter_action are required for create action")?;
            let fa = fa
                .filter(|v| !v.is_empty())
                .ok_or("criteria and filter_action are required for create action")?;
            let v = api(
                &session,
                "manage_gmail_filter",
                Method::POST,
                &format!("{API}/settings/filters"),
                &[],
                Some(&json!({"criteria":criteria,"action":fa})),
                Retry::Never,
            )
            .await?;
            Ok(format!(
                "Filter created successfully!\nFilter ID: {}",
                s(&v, "id").unwrap_or("(unknown)")
            ))
        }
        "delete" => {
            let id = id
                .filter(|v| !v.is_empty())
                .ok_or("filter_id is required for delete action")?;
            let url = format!("{API}/settings/filters/{}", path_id(&id)?);
            let old = api(
                &session,
                "manage_gmail_filter",
                Method::GET,
                &url,
                &[],
                None,
                Retry::Safe,
            )
            .await?;
            if let Err(error) = api(
                &session,
                "manage_gmail_filter",
                Method::DELETE,
                &url,
                &[],
                None,
                Retry::Safe,
            )
            .await
                && !error.contains("404")
            {
                return Err(error);
            }
            Ok(format!(
                "Filter deleted successfully!\nFilter ID: {id}\nCriteria: {}\nAction: {}",
                old.get("criteria").unwrap_or(&Value::Null),
                old.get("action").unwrap_or(&Value::Null)
            ))
        }
        _ => Err(format!(
            "Invalid action '{action}'. Must be 'create' or 'delete'."
        )),
    }
}

async fn modify_labels(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, MODIFY_ARGS)?;
    let id = req_str(a, "message_id")?;
    let add = str_list(a, "add_label_ids", true)?.unwrap_or_default();
    let remove = str_list(a, "remove_label_ids", true)?.unwrap_or_default();
    if add.is_empty() && remove.is_empty() {
        return Err("At least one of add_label_ids or remove_label_ids must be provided.".into());
    }
    let session = auth(a, MODIFY_SCOPES).await?;
    api(
        &session,
        "modify_gmail_message_labels",
        Method::POST,
        &format!("{API}/messages/{}/modify", path_id(&id)?),
        &[],
        Some(&json!({"addLabelIds":add,"removeLabelIds":remove})),
        Retry::Safe,
    )
    .await?;
    Ok(format!(
        "Message labels updated successfully!\nMessage ID: {id}\n{}",
        label_actions(&add, &remove)
    ))
}
async fn batch_modify_labels(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, BATCH_MODIFY_ARGS)?;
    let ids = str_list(a, "message_ids", false)?.unwrap_or_default();
    if ids.is_empty() {
        return Err("No message IDs provided".into());
    }
    let add = str_list(a, "add_label_ids", true)?.unwrap_or_default();
    let remove = str_list(a, "remove_label_ids", true)?.unwrap_or_default();
    if add.is_empty() && remove.is_empty() {
        return Err("At least one of add_label_ids or remove_label_ids must be provided.".into());
    }
    let session = auth(a, MODIFY_SCOPES).await?;
    for chunk in ids.chunks(CHUNK) {
        api(
            &session,
            "batch_modify_gmail_message_labels",
            Method::POST,
            &format!("{API}/messages/batchModify"),
            &[],
            Some(&json!({"ids":chunk,"addLabelIds":add,"removeLabelIds":remove})),
            Retry::Safe,
        )
        .await?;
    }
    Ok(format!(
        "Labels updated for {} messages: {}",
        ids.len(),
        label_actions(&add, &remove)
    ))
}

#[derive(Clone, Copy)]
enum Retry {
    Never,
    Safe,
}
async fn auth(a: &Map<String, Value>, sets: &[&[&str]]) -> Result<GoogleSession, String> {
    let supplied = req_str(a, "user_google_email")?;
    let mut first = None;
    for scopes in sets {
        match GoogleSession::authenticate(scopes).await {
            Ok(s) => {
                if !supplied.eq_ignore_ascii_case(&s.user_email) {
                    return Err(
                        "user_google_email does not match the configured USER_GOOGLE_EMAIL".into(),
                    );
                }
                return Ok(s);
            }
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(first.unwrap_or_else(|| "No Google scopes were configured".into()))
}
async fn api(
    session: &GoogleSession,
    tool: &str,
    method: Method,
    url: &str,
    query: &[(String, String)],
    body: Option<&Value>,
    retry: Retry,
) -> Result<Value, String> {
    if !url.starts_with(API) {
        return Err("Refusing non-Gmail API endpoint".into());
    }
    let attempts = if matches!(retry, Retry::Never) { 1 } else { 4 };
    let mut last = String::new();
    for attempt in 0..attempts {
        let mut request = session.request(method.clone(), url);
        if !query.is_empty() {
            request = request.query(query)
        }
        if let Some(b) = body {
            request = request.json(b)
        }
        let result = async {
            let response = request
                .send()
                .await
                .map_err(|_| format!("API error in {tool}: Google API request failed"))?;
            let status = response.status();
            let response_body = response
                .text()
                .await
                .map_err(|_| format!("API error in {tool}: failed to read Google API response"))?;
            if !status.is_success() {
                let parsed = serde_json::from_str::<Value>(&response_body).ok();
                let message = parsed
                    .as_ref()
                    .and_then(|value| value.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Google API request failed");
                let reasons = parsed
                    .as_ref()
                    .and_then(|value| value.pointer("/error/errors"))
                    .and_then(Value::as_array)
                    .map(|errors| {
                        errors
                            .iter()
                            .filter_map(|error| error.get("reason").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "API error in {tool}: {status}: {message} {reasons}"
                ));
            }
            if response_body.trim().is_empty() {
                return Ok(Value::Null);
            }
            serde_json::from_str(&response_body)
                .map_err(|_| format!("API error in {tool}: Google API returned invalid JSON"))
        }
        .await;
        match result {
            Ok(v) => return Ok(v),
            Err(e) => {
                let transient = [
                    "429",
                    "500",
                    "502",
                    "503",
                    "504",
                    "quota",
                    "rate limit",
                    "ratelimit",
                    "dailylimit",
                    "google api request failed",
                ]
                .iter()
                .any(|m| e.to_ascii_lowercase().contains(m));
                last = e;
                if !transient || attempt + 1 == attempts {
                    break;
                }
                sleep(Duration::from_secs(1 << attempt)).await;
            }
        }
    }
    Err(last)
}
async fn sleep(d: Duration) {
    let _ = tokio::task::spawn_blocking(move || std::thread::sleep(d)).await;
}
async fn fetch_message(ses: &GoogleSession, id: &str, fmt: &str) -> Result<Value, String> {
    let mut q = vec![("format".into(), fmt.into())];
    if fmt == "metadata" {
        q.extend(
            HEADERS
                .iter()
                .map(|h| ("metadataHeaders".into(), (*h).into())),
        )
    }
    api(
        ses,
        "get_gmail_message_content",
        Method::GET,
        &format!("{API}/messages/{}", path_id(id)?),
        &q,
        None,
        Retry::Safe,
    )
    .await
}
async fn fetch_thread(ses: &GoogleSession, id: &str) -> Result<Value, String> {
    api(
        ses,
        "get_gmail_thread_content",
        Method::GET,
        &format!("{API}/threads/{}", path_id(id)?),
        &[("format".into(), "full".into())],
        None,
        Retry::Safe,
    )
    .await
}
fn path_id(v: &str) -> Result<String, String> {
    if v.is_empty() || v.chars().any(char::is_control) {
        Err("Gmail resource ID must be non-empty and control-character-free".into())
    } else {
        Ok(utf8_percent_encode(v, NON_ALPHANUMERIC).to_string())
    }
}
fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}
fn headers(p: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for h in arr(p, "headers") {
        let (Some(n), Some(v)) = (s(h, "name"), s(h, "value")) else {
            continue;
        };
        if let Some(c) = HEADERS.iter().find(|x| x.eq_ignore_ascii_case(n)) {
            if matches!(c.to_ascii_lowercase().as_str(), "to" | "cc") && out.contains_key(*c) {
                out.entry((*c).into()).and_modify(|x: &mut String| {
                    x.push_str(", ");
                    x.push_str(v)
                });
            } else {
                out.insert((*c).into(), v.into());
            }
        }
    }
    out
}
fn hv<'a>(h: &'a HashMap<String, String>, k: &str, d: &'a str) -> &'a str {
    h.get(k).map(String::as_str).unwrap_or(d)
}
fn header_lines(h: &HashMap<String, String>, id: Option<&str>) -> Vec<String> {
    let mut o = Vec::new();
    if let Some(id) = id {
        o.push(format!("Message ID: {id}"))
    }
    o.extend([
        format!("Subject: {}", hv(h, "Subject", "(no subject)")),
        format!("From: {}", hv(h, "From", "(unknown sender)")),
        format!("Date: {}", hv(h, "Date", "(unknown date)")),
    ]);
    for k in ["Message-ID", "In-Reply-To", "References"] {
        if let Some(v) = h.get(k).filter(|v| !v.is_empty()) {
            o.push(format!("{k}: {v}"))
        }
    }
    o.push(
        h.get("To")
            .map(|v| format!("To: {v}"))
            .unwrap_or_else(|| "To: [not present in Gmail response]".into()),
    );
    o.push(
        h.get("Cc")
            .map(|v| format!("Cc: {v}"))
            .unwrap_or_else(|| "Cc: [not present in Gmail response]".into()),
    );
    for k in ["List-Unsubscribe", "Precedence", "List-Id"] {
        if let Some(v) = h.get(k).filter(|v| !v.is_empty()) {
            o.push(format!("{k}: {v}"))
        }
    }
    o
}
fn bodies(p: &Value) -> (String, String) {
    let mut plain = String::new();
    let mut html = String::new();
    let mut queue = vec![p];
    while let Some(part) = queue.pop() {
        if let Some(parts) = part.get("parts").and_then(Value::as_array) {
            queue.extend(parts.iter().rev())
        }
        let mime = s(part, "mimeType").unwrap_or("");
        let Some(data) = part.pointer("/body/data").and_then(Value::as_str) else {
            continue;
        };
        let Ok(bytes) = b64decode(data, true, usize::MAX) else {
            continue;
        };
        let value = String::from_utf8_lossy(&bytes).into_owned();
        if mime == "text/plain" && plain.is_empty() {
            plain = value
        } else if mime == "text/html" && html.is_empty() {
            html = value
        }
    }
    (plain, html)
}
#[derive(Clone)]
struct AttMeta {
    filename: String,
    mime: String,
    size: u64,
    id: String,
}
fn attachment_metas(p: &Value) -> Vec<AttMeta> {
    fn walk(v: &Value, o: &mut Vec<AttMeta>) {
        if let (Some(name), Some(id)) = (
            s(v, "filename").filter(|v| !v.is_empty()),
            v.pointer("/body/attachmentId").and_then(Value::as_str),
        ) {
            o.push(AttMeta {
                filename: name.into(),
                mime: s(v, "mimeType")
                    .unwrap_or("application/octet-stream")
                    .into(),
                size: v.pointer("/body/size").and_then(Value::as_u64).unwrap_or(0),
                id: id.into(),
            })
        }
        if let Some(parts) = v.get("parts").and_then(Value::as_array) {
            for p in parts {
                walk(p, o)
            }
        }
    }
    let mut o = Vec::new();
    walk(p, &mut o);
    o
}
fn format_body(plain: &str, html: &str, fmt: &str) -> String {
    if fmt == "html" {
        if !html.trim().is_empty() {
            return truncate(html.trim());
        }
        return if plain.trim().is_empty() {
            "[No readable content found]".into()
        } else {
            plain.trim().into()
        };
    }
    let ht = html_text(html);
    let pn = plain
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let hn = ht
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let low = [
        "your client does not support html",
        "view this email in your browser",
        "open this email in your browser",
    ]
    .iter()
    .any(|x| pn.contains(x))
        || (hn.len() >= pn.len() + 80 && hn.ends_with(&pn));
    if !ht.trim().is_empty() && (plain.trim().is_empty() || plain.contains("<!--") || low) {
        truncate(ht.trim())
    } else if !plain.trim().is_empty() {
        plain.into()
    } else {
        "[No readable content found]".into()
    }
}
fn format_message(v: &Value, id: &str, fmt: &str, raw: Option<&str>, metadata: bool) -> String {
    let p = v.get("payload").unwrap_or(&Value::Null);
    let h = headers(p);
    let mut o = header_lines(&h, Some(id));
    o.push(format!("Web Link: {}", web(id)));
    if metadata {
        return o.join("\n");
    }
    if fmt == "raw" {
        o.push(format!(
            "\n--- RAW MIME ---\n{}",
            raw.unwrap_or("[No raw content found]")
        ))
    } else {
        let (pl, ht) = bodies(p);
        o.push(format!("\n--- BODY ---\n{}", format_body(&pl, &ht, fmt)))
    }
    let at = attachment_metas(p);
    if !at.is_empty() {
        o.push("\n--- ATTACHMENTS ---".into());
        for (i, x) in at.iter().enumerate() {
            o.push(format!("{}. {} ({}, {:.1} KB)\n   Attachment ID: {}\n   Use get_gmail_attachment_content(message_id='{id}', attachment_id='{}') to download",i+1,x.filename,x.mime,x.size as f64/1024.0,x.id,x.id))
        }
    }
    o.join("\n")
}
async fn format_thread(session: &GoogleSession, t: &Value, id: &str, fmt: &str) -> String {
    let ms = arr(t, "messages");
    if ms.is_empty() {
        return format!("No messages found in thread '{id}'.");
    }
    let first = headers(ms[0].get("payload").unwrap_or(&Value::Null));
    let subject = hv(&first, "Subject", "(no subject)");
    let mut o = vec![
        format!("Thread ID: {id}"),
        format!("Subject: {subject}"),
        format!("Messages: {}", ms.len()),
        "".into(),
    ];
    for (i, m) in ms.iter().enumerate() {
        let mid = s(m, "id").unwrap_or("");
        let p = m.get("payload").unwrap_or(&Value::Null);
        let h = headers(p);
        o.extend([
            format!("=== Message {} ===", i + 1),
            format!("From: {}", hv(&h, "From", "(unknown sender)")),
            format!("Date: {}", hv(&h, "Date", "(unknown date)")),
            h.get("To")
                .map(|v| format!("To: {v}"))
                .unwrap_or_else(|| "To: [not present in Gmail response]".into()),
            h.get("Cc")
                .map(|v| format!("Cc: {v}"))
                .unwrap_or_else(|| "Cc: [not present in Gmail response]".into()),
        ]);
        for k in ["Message-ID", "In-Reply-To", "References"] {
            if let Some(v) = h.get(k).filter(|v| !v.is_empty()) {
                o.push(format!("{k}: {v}"))
            }
        }
        if hv(&h, "Subject", "(no subject)") != subject {
            o.push(format!("Subject: {}", hv(&h, "Subject", "(no subject)")))
        }
        if fmt == "raw" {
            let content = match fetch_message(session, mid, "raw").await {
                Ok(v) => b64decode(s(&v, "raw").unwrap_or(""), true, usize::MAX)
                    .map(|b| truncate(&String::from_utf8_lossy(&b)))
                    .unwrap_or_else(|e| format!("[Failed to decode raw MIME: {e}]")),
                Err(e) => format!("[Failed to fetch raw MIME: {e}]"),
            };
            o.push(format!("\n--- RAW MIME ---\n{content}\n"))
        } else {
            let (pl, ht) = bodies(p);
            o.extend(["".into(), format_body(&pl, &ht, fmt), "".into()])
        }
        let at = attachment_metas(p);
        if !at.is_empty() {
            o.push("--- ATTACHMENTS ---".into());
            for (j, x) in at.iter().enumerate() {
                o.push(format!("{}. {} ({}, {:.1} KB)\n   Attachment ID: {}\n   Use get_gmail_attachment_content(message_id='{mid}', attachment_id='{}') to download",j+1,x.filename,x.mime,x.size as f64/1024.0,x.id,x.id))
            }
            o.push("".into())
        }
    }
    o.join("\n")
}
fn web(id: &str) -> String {
    format!("https://mail.google.com/mail/u/0/#all/{id}")
}
fn truncate(v: &str) -> String {
    if v.chars().count() <= MAX_BODY {
        v.into()
    } else {
        format!(
            "{}\n\n[Content truncated...]",
            v.chars().take(MAX_BODY).collect::<String>()
        )
    }
}

fn analyze(t: &Value, user: &str) -> Value {
    let ms = arr(t, "messages");
    let me = normalize_email(user);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut participants = HashSet::new();
    let mut non_draft = HashSet::new();
    let mut excluded = 0;
    let mut last: Option<(i64, String, String)> = None;
    let subject = ms
        .first()
        .map(|m| headers(m.get("payload").unwrap_or(&Value::Null)))
        .and_then(|h| h.get("Subject").cloned());
    for m in ms {
        let h = headers(m.get("payload").unwrap_or(&Value::Null));
        let draft = arr(m, "labelIds")
            .iter()
            .any(|v| v.as_str() == Some("DRAFT"));
        let mut current = HashSet::new();
        for k in ["From", "To", "Cc"] {
            for addr in split_addresses(hv(&h, k, "")) {
                let n = normalize_email(&addr);
                if n.contains('@') {
                    participants.insert(n.clone());
                    current.insert(n);
                }
            }
        }
        if draft {
            excluded += 1;
            continue;
        }
        non_draft.extend(current);
        let raw = hv(&h, "From", "").to_owned();
        let sender = normalize_email(&raw);
        if sender.contains('@') {
            *counts.entry(sender.clone()).or_default() += 1
        }
        let Some(ts) = s(m, "internalDate").and_then(|v| v.parse::<i64>().ok()) else {
            continue;
        };
        if last.as_ref().is_none_or(|(x, _, _)| ts >= *x) {
            last = Some((ts, raw, sender));
        }
    }
    let external = non_draft.iter().any(|v| v != &me);
    let (ls, lt, ball) = match last {
        Some((ts, raw, n)) => {
            let timestamp = time::OffsetDateTime::from_unix_timestamp(ts.div_euclid(1000))
                .ok()
                .and_then(|value| {
                    value
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .map(Value::String)
                .unwrap_or(Value::Null);
            (
                Value::String(raw),
                timestamp,
                if !me.contains('@') || !n.contains('@') || !external {
                    Value::Null
                } else if n == me {
                    json!("them")
                } else {
                    json!("user")
                },
            )
        }
        None => (Value::Null, Value::Null, Value::Null),
    };
    let mut p: Vec<_> = participants.into_iter().collect();
    p.sort();
    json!({"thread_id":s(t,"id").unwrap_or(""),"thread_subject":subject,"last_sender":ls,"last_timestamp":lt,"ball_in_court_of":ball,"message_count_by_sender":counts,"participants":p,"excluded_drafts":excluded,"message_count":ms.len()})
}
fn normalize_email(v: &str) -> String {
    let x = v
        .rsplit_once('<')
        .map(|(_, r)| r.trim_end_matches('>').trim())
        .unwrap_or(v)
        .trim()
        .to_ascii_lowercase();
    match x.split_once('@') {
        Some((l, d)) => format!("{}@{d}", l.split('+').next().unwrap_or(l)),
        None => x,
    }
}
fn html_text(v: &str) -> String {
    let mut out = String::new();
    let mut tag = String::new();
    let mut inside = false;
    let mut skip = 0i32;
    for c in v.chars() {
        if c == '<' {
            inside = true;
            tag.clear();
            continue;
        }
        if inside {
            if c == '>' {
                inside = false;
                let n = tag.trim().to_ascii_lowercase();
                let name = n
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if matches!(name, "script" | "style") {
                    if n.starts_with('/') {
                        skip = (skip - 1).max(0)
                    } else {
                        skip += 1
                    }
                }
                if skip == 0
                    && (n == "br"
                        || n.starts_with("br ")
                        || (n.starts_with('/')
                            && matches!(
                                name,
                                "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "blockquote"
                            )))
                {
                    out.push('\n')
                }
            } else {
                tag.push(c)
            }
        } else if skip == 0 {
            out.push(c)
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let mut lines = Vec::new();
    for line in decoded.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() || lines.last().is_some_and(|x: &String| !x.is_empty()) {
            lines.push(line)
        }
    }
    lines.join("\n").trim().into()
}
fn escape_html(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Default)]
struct Compose {
    owner: String,
    to: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    body_format: String,
    cc: Option<String>,
    bcc: Option<String>,
    from_name: Option<String>,
    from_email: String,
    thread_id: Option<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
    specs: Vec<Map<String, Value>>,
    attachments: Vec<Resolved>,
    requested: usize,
    include_signature: bool,
    quote_original: bool,
    reply_all: bool,
}
#[derive(Clone)]
struct Resolved {
    bytes: Vec<u8>,
    filename: String,
    mime: String,
    cid: Option<String>,
}
impl Compose {
    fn from_args(a: &Map<String, Value>, owner: &str, draft: bool) -> Result<Self, String> {
        let specs = dict_list(a, "attachments")?.unwrap_or_default();
        let c = Self {
            owner: owner.into(),
            to: opt_str(a, "to")?,
            subject: if draft {
                Some(req_str(a, "subject")?)
            } else {
                opt_str(a, "subject")?
            },
            body: if draft {
                Some(req_str(a, "body")?)
            } else {
                opt_str(a, "body")?
            },
            body_format: enum_arg(a, "body_format", "plain", &["plain", "html"])?,
            cc: opt_str(a, "cc")?,
            bcc: opt_str(a, "bcc")?,
            from_name: opt_str(a, "from_name")?,
            from_email: opt_str(a, "from_email")?.unwrap_or_else(|| owner.into()),
            thread_id: opt_str(a, "thread_id")?,
            in_reply_to: opt_str(a, "in_reply_to")?,
            references: opt_str(a, "references")?,
            requested: specs.len(),
            specs,
            attachments: Vec::new(),
            include_signature: bool_arg(a, "include_signature", true)?,
            quote_original: bool_arg(a, "quote_original", false)?,
            reply_all: if draft {
                false
            } else {
                bool_arg(a, "reply_all", false)?
            },
        };
        c.validate()?;
        Ok(c)
    }
    fn validate(&self) -> Result<(), String> {
        for (n, v) in [
            ("subject", self.subject.as_deref()),
            ("from_name", self.from_name.as_deref()),
            ("from_email", Some(self.from_email.as_str())),
            ("in_reply_to", self.in_reply_to.as_deref()),
            ("references", self.references.as_deref()),
        ] {
            if let Some(v) = v {
                no_ctl(v, n)?
            }
        }
        validate_addresses(&self.from_email, "from_email")?;
        for (n, v) in [
            ("to", self.to.as_deref()),
            ("cc", self.cc.as_deref()),
            ("bcc", self.bcc.as_deref()),
        ] {
            if let Some(v) = v.filter(|v| !v.trim().is_empty()) {
                validate_addresses(v, n)?
            }
        }
        Ok(())
    }
    async fn resolve(&mut self) -> Result<(), String> {
        let mut total = self
            .attachments
            .iter()
            .map(|x| x.bytes.len())
            .sum::<usize>();
        for spec in std::mem::take(&mut self.specs) {
            if let Ok(item) = resolve_attachment(spec, &self.owner).await {
                total = total.saturating_add(item.bytes.len());
                if total > MAX_ATTACHMENT {
                    return Err(format!(
                        "Attachments total exceeds {MAX_ATTACHMENT} bytes ({total} bytes)"
                    ));
                }
                self.attachments.push(item)
            }
        }
        Ok(())
    }
}

async fn forward(
    session: &GoogleSession,
    c: &mut Compose,
    mid: &str,
    include: bool,
) -> Result<(), String> {
    let to =
        c.to.as_deref()
            .filter(|v| !v.is_empty())
            .ok_or("'to' is required when forwarding via 'forward_message_id'.")?;
    validate_addresses(to, "to")?;
    let original = fetch_message(session, mid, "full").await?;
    let p = original.get("payload").unwrap_or(&Value::Null);
    let h = headers(p);
    let (pl, ht) = bodies(p);
    let os = hv(&h, "Subject", "(no subject)");
    if c.subject.is_none() {
        c.subject = Some(
            if os.trim_start().to_ascii_lowercase().starts_with("fwd:")
                || os.trim_start().to_ascii_lowercase().starts_with("fw:")
            {
                os.into()
            } else {
                format!("Fwd: {os}")
            },
        )
    }
    let note = c.body.clone().unwrap_or_default();
    if !ht.trim().is_empty() || (c.body_format == "html" && !note.is_empty()) {
        let note = if note.is_empty() {
            String::new()
        } else if c.body_format == "html" {
            format!("<div>{note}</div><br/>")
        } else {
            format!(
                "<div>{}</div><br/>",
                escape_html(&note).replace('\n', "<br/>")
            )
        };
        let original = if ht.trim().is_empty() {
            escape_html(&pl).replace('\n', "<br/>")
        } else {
            ht
        };
        c.body = Some(format!(
            "{note}<div style=\"border-left:1px solid #ccc;padding-left:10px\"><div>---------- Forwarded message ---------<br/>From: {}<br/>Date: {}<br/>Subject: {}<br/>To: {}</div><br/>{original}</div>",
            escape_html(hv(&h, "From", "(unknown sender)")),
            escape_html(hv(&h, "Date", "(unknown date)")),
            escape_html(os),
            escape_html(hv(&h, "To", ""))
        ));
        c.body_format = "html".into()
    } else {
        let note = if note.is_empty() {
            String::new()
        } else {
            format!("{note}\n\n")
        };
        c.body = Some(format!(
            "{note}---------- Forwarded message ---------\nFrom: {}\nDate: {}\nSubject: {os}\nTo: {}\n\n{pl}",
            hv(&h, "From", "(unknown sender)"),
            hv(&h, "Date", "(unknown date)"),
            hv(&h, "To", "")
        ));
        c.body_format = "plain".into()
    }
    c.thread_id = None;
    c.in_reply_to = None;
    c.references = None;
    c.include_signature = false;
    c.quote_original = false;
    if include {
        for x in attachment_metas(p) {
            let url = format!(
                "{API}/messages/{}/attachments/{}",
                path_id(mid)?,
                path_id(&x.id)?
            );
            let v = api(
                session,
                "send_gmail_message",
                Method::GET,
                &url,
                &[],
                None,
                Retry::Safe,
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to include requested attachment '{}': {e}",
                    x.filename
                )
            })?;
            c.attachments.push(Resolved {
                bytes: b64decode(s(&v, "data").unwrap_or(""), true, MAX_ATTACHMENT)?,
                filename: x.filename,
                mime: x.mime,
                cid: None,
            })
        }
    }
    Ok(())
}

async fn enrich(session: &GoogleSession, c: &mut Compose, require_to: bool) -> Result<(), String> {
    let context = if let Some(id) = c.thread_id.as_deref()
        && (c.quote_original
            || c.reply_all
            || c.in_reply_to.is_none()
            || c.references.is_none()
            || c.to.is_none())
    {
        fetch_thread(session, id).await.ok()
    } else {
        None
    };
    let target = context.as_ref().and_then(|t| {
        let ms = arr(t, "messages");
        let desired = c.in_reply_to.as_deref();
        ms.iter()
            .filter(|m| {
                !arr(m, "labelIds")
                    .iter()
                    .any(|v| v.as_str() == Some("TRASH"))
            })
            .find(|m| {
                desired.is_some_and(|d| {
                    headers(m.get("payload").unwrap_or(&Value::Null))
                        .get("Message-ID")
                        .is_some_and(|x| x == d)
                })
            })
            .or_else(|| {
                ms.iter().rev().find(|m| {
                    !arr(m, "labelIds")
                        .iter()
                        .any(|v| v.as_str() == Some("TRASH"))
                })
            })
            .cloned()
    });
    let ids: Vec<String> = context
        .as_ref()
        .map(|t| {
            arr(t, "messages")
                .iter()
                .filter(|m| {
                    !arr(m, "labelIds")
                        .iter()
                        .any(|v| v.as_str() == Some("TRASH"))
                })
                .filter_map(|m| {
                    headers(m.get("payload").unwrap_or(&Value::Null))
                        .get("Message-ID")
                        .cloned()
                })
                .collect()
        })
        .unwrap_or_default();
    derive_headers(c, &ids);
    if let Some(t) = target.as_ref() {
        let h = headers(t.get("payload").unwrap_or(&Value::Null));
        if c.to.is_none() {
            c.to = h.get("Reply-To").or_else(|| h.get("From")).cloned()
        }
        if c.reply_all {
            derive_all(c, &h, &session.user_email)
        }
        if c.subject.as_deref().is_some_and(|v| v.trim().is_empty()) {
            c.subject = h.get("Subject").cloned()
        }
    }
    let signature = if c.include_signature {
        signature(session, &c.from_email).await?
    } else {
        String::new()
    };
    let body = c.body.clone().unwrap_or_default();
    c.body = Some(if c.quote_original {
        if let Some(t) = target.as_ref() {
            let p = t.get("payload").unwrap_or(&Value::Null);
            let h = headers(p);
            let (pl, ht) = bodies(p);
            quote(
                &body,
                &c.body_format,
                &signature,
                hv(&h, "From", "unknown"),
                hv(&h, "Date", ""),
                &pl,
                &ht,
            )
        } else {
            append_signature(&body, &c.body_format, &signature)
        }
    } else {
        append_signature(&body, &c.body_format, &signature)
    });
    if require_to && c.to.as_deref().is_none_or(|v| v.trim().is_empty()) {
        return Err("Could not derive a recipient. Pass 'to' explicitly.".into());
    }
    c.validate()
}
fn derive_headers(c: &mut Compose, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    if c.in_reply_to.is_none() {
        c.in_reply_to = c
            .references
            .as_deref()
            .and_then(|v| message_ids(v).last().cloned())
            .or_else(|| ids.last().cloned())
    }
    if c.references.is_none()
        && let Some(reply) = &c.in_reply_to
    {
        c.references = Some(if let Some(i) = ids.iter().position(|x| x == reply) {
            ids[..=i].join(" ")
        } else {
            reply.clone()
        })
    }
}
fn message_ids(v: &str) -> Vec<String> {
    let mut o = Vec::new();
    let mut rest = v;
    while let Some(start) = rest.find('<') {
        let x = &rest[start..];
        let Some(end) = x.find('>') else { break };
        o.push(x[..=end].into());
        rest = &x[end + 1..]
    }
    if o.is_empty() {
        o.extend(v.split_whitespace().map(str::to_owned))
    }
    o
}
fn derive_all(c: &mut Compose, h: &HashMap<String, String>, authenticated: &str) {
    let excluded = [
        normalize_email(authenticated),
        normalize_email(&c.from_email),
    ];
    let sender = h
        .get("Reply-To")
        .or_else(|| h.get("From"))
        .map(String::as_str)
        .unwrap_or("");
    if c.to.as_deref().is_none_or(|v| v.trim().is_empty()) {
        let v: Vec<_> = split_addresses(sender)
            .into_iter()
            .filter(|v| !excluded.contains(&normalize_email(v)))
            .collect();
        c.to = (!v.is_empty()).then(|| v.join(", "))
    }
    if c.cc.is_none() {
        let mut seen: HashSet<_> = excluded.into_iter().collect();
        if let Some(to) = &c.to {
            seen.extend(split_addresses(to).into_iter().map(|v| normalize_email(&v)))
        }
        let mut others = Vec::new();
        for v in [sender, hv(h, "To", ""), hv(h, "Cc", "")] {
            for x in split_addresses(v) {
                if seen.insert(normalize_email(&x)) {
                    others.push(x)
                }
            }
        }
        c.cc = (!others.is_empty()).then(|| others.join(", "))
    }
}
async fn signature(session: &GoogleSession, from: &str) -> Result<String, String> {
    let v = match api(
        session,
        "gmail_signature",
        Method::GET,
        &format!("{API}/settings/sendAs"),
        &[],
        None,
        Retry::Safe,
    )
    .await
    {
        Ok(v) => v,
        Err(e)
            if e.contains("401")
                || (e.contains("403")
                    && ![
                        "quota",
                        "rate limit",
                        "rateLimitExceeded",
                        "userRateLimitExceeded",
                        "dailyLimitExceeded",
                    ]
                    .iter()
                    .any(|marker| {
                        e.to_ascii_lowercase()
                            .contains(&marker.to_ascii_lowercase())
                    })) =>
        {
            return Ok(String::new());
        }
        Err(e) => return Err(format!("Failed to fetch Gmail send-as signatures: {e}")),
    };
    Ok(arr(&v, "sendAs")
        .iter()
        .find(|v| s(v, "sendAsEmail").is_some_and(|x| x.eq_ignore_ascii_case(from)))
        .or_else(|| {
            arr(&v, "sendAs")
                .iter()
                .find(|v| v.get("isPrimary").and_then(Value::as_bool) == Some(true))
        })
        .or_else(|| arr(&v, "sendAs").first())
        .and_then(|v| s(v, "signature"))
        .unwrap_or("")
        .into())
}
fn append_signature(body: &str, fmt: &str, sig: &str) -> String {
    if sig.trim().is_empty() {
        body.into()
    } else {
        format!(
            "{body}{}{sig}",
            if body.trim().is_empty() {
                ""
            } else if fmt == "html" {
                "<br><br>"
            } else {
                "\n\n"
            },
            sig = if fmt == "html" {
                sig.into()
            } else {
                html_text(sig)
            }
        )
    }
}
fn quote(
    body: &str,
    fmt: &str,
    sig: &str,
    sender: &str,
    date: &str,
    plain: &str,
    html: &str,
) -> String {
    let attribution = if date.is_empty() {
        format!("{sender} wrote:")
    } else {
        format!("On {date}, {sender} wrote:")
    };
    if fmt == "html" {
        let sig = if sig.trim().is_empty() {
            String::new()
        } else {
            format!("<br><br>{sig}")
        };
        let original = if html.trim().is_empty() {
            format!("<pre>{}</pre>", escape_html(plain))
        } else {
            html.into()
        };
        format!(
            "{body}{sig}<br><br><div class=\"gmail_quote\"><span>{}</span><br><blockquote style=\"margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex\">{original}</blockquote></div>",
            escape_html(&attribution)
        )
    } else {
        let sig = if sig.trim().is_empty() {
            String::new()
        } else {
            format!("\n\n{}", html_text(sig))
        };
        let original = if plain.trim().is_empty() {
            html_text(html)
        } else {
            plain.into()
        };
        let quoted = original
            .lines()
            .map(|x| format!("> {x}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{body}{sig}\n\n{attribution}\n{quoted}")
    }
}

fn mime_message(c: &Compose) -> Result<String, String> {
    c.validate()?;
    let mut h = Vec::new();
    let mut subject = c.subject.clone().unwrap_or_default();
    if c.in_reply_to.is_some() && !subject.to_ascii_lowercase().starts_with("re:") {
        subject = format!("Re: {subject}")
    }
    h.push(format!("Subject: {subject}"));
    h.push(format!(
        "From: {}",
        if let Some(n) = c.from_name.as_deref().filter(|v| !v.is_empty()) {
            format!(
                "\"{}\" <{}>",
                n.replace('\\', "\\\\").replace('"', "\\\""),
                c.from_email
            )
        } else {
            c.from_email.clone()
        }
    ));
    for (n, v) in [
        ("To", c.to.as_deref()),
        ("Cc", c.cc.as_deref()),
        ("Bcc", c.bcc.as_deref()),
        ("In-Reply-To", c.in_reply_to.as_deref()),
        ("References", c.references.as_deref()),
    ] {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            h.push(format!("{n}: {v}"))
        }
    }
    h.push("MIME-Version: 1.0".into());
    let body = c.body.as_deref().unwrap_or("");
    let mut entity = if c.body_format == "html" {
        let b = boundary("alternative");
        format!(
            "Content-Type: multipart/alternative; boundary=\"{b}\"\r\n\r\n--{b}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}\r\n--{b}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n--{b}--\r\n",
            html_text(body)
        )
    } else {
        format!(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n"
        )
    };
    let inline: Vec<_> = c.attachments.iter().filter(|x| x.cid.is_some()).collect();
    if !inline.is_empty() {
        let b = boundary("related");
        let mut x =
            format!("Content-Type: multipart/related; boundary=\"{b}\"\r\n\r\n--{b}\r\n{entity}");
        for a in inline {
            x.push_str(&att_part(a, &b, true))
        }
        x.push_str(&format!("--{b}--\r\n"));
        entity = x
    }
    let regular: Vec<_> = c.attachments.iter().filter(|x| x.cid.is_none()).collect();
    if !regular.is_empty() {
        let b = boundary("mixed");
        let mut x =
            format!("Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n--{b}\r\n{entity}");
        for a in regular {
            x.push_str(&att_part(a, &b, false))
        }
        x.push_str(&format!("--{b}--\r\n"));
        entity = x
    }
    Ok(format!("{}\r\n{entity}", h.join("\r\n")))
}
fn boundary(kind: &str) -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let s = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("=_workspace_{kind}_{n:x}_{s:x}")
}
fn att_part(a: &Resolved, b: &str, inline: bool) -> String {
    let disposition = if inline { "inline" } else { "attachment" };
    let cid = a
        .cid
        .as_deref()
        .map(|x| format!("Content-ID: <{x}>\r\n"))
        .unwrap_or_default();
    format!(
        "--{b}\r\nContent-Type: {}; name=\"{}\"\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: {disposition}; filename=\"{}\"\r\n{cid}\r\n{}\r\n",
        a.mime,
        header_param(&a.filename),
        header_param(&a.filename),
        wrap76(&b64encode(&a.bytes, false))
    )
}
fn header_param(v: &str) -> String {
    v.replace(['\\', '"'], "_")
}
fn wrap76(v: &str) -> String {
    v.as_bytes()
        .chunks(76)
        .map(|x| String::from_utf8_lossy(x).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}
fn no_ctl(v: &str, n: &str) -> Result<(), String> {
    if v.chars().any(char::is_control) {
        Err(format!("{n} contains invalid control characters"))
    } else {
        Ok(())
    }
}
fn split_addresses(v: &str) -> Vec<String> {
    let mut addresses = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut angle_depth = 0_u8;
    for character in v.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quoted {
            current.push(character);
            escaped = true;
            continue;
        }
        match character {
            '"' => quoted = !quoted,
            '<' if !quoted => angle_depth = angle_depth.saturating_add(1),
            '>' if !quoted => angle_depth = angle_depth.saturating_sub(1),
            ',' if !quoted && angle_depth == 0 => {
                let address = current.trim();
                if !address.is_empty() {
                    addresses.push(address.to_owned());
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    let address = current.trim();
    if !address.is_empty() {
        addresses.push(address.to_owned());
    }
    addresses
}
fn validate_addresses(v: &str, n: &str) -> Result<(), String> {
    no_ctl(v, n)?;
    for x in split_addresses(v) {
        let email = x
            .rsplit_once('<')
            .map(|(_, r)| r.trim_end_matches('>').trim())
            .unwrap_or(&x);
        let valid = email.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty()
                && d.contains('.')
                && !d.starts_with('.')
                && !d.ends_with('.')
                && !email.chars().any(char::is_whitespace)
        });
        if !valid {
            return Err(format!("{n} contains an invalid email address"));
        }
    }
    Ok(())
}
fn content_id(v: &str) -> Result<String, String> {
    no_ctl(v, "content_id")?;
    let x = v.trim().trim_matches(['<', '>']).trim();
    if x.is_empty()
        || !x
            .chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphanumeric() || (i > 0 && "._+-@".contains(c)))
    {
        Err("content_id contains invalid characters; use letters, digits, '.', '_', '+', '-', or '@'".into())
    } else {
        Ok(x.into())
    }
}

async fn resolve_attachment(spec: Map<String, Value>, owner: &str) -> Result<Resolved, String> {
    let count = ["url", "path", "content"]
        .iter()
        .filter(|k| spec.get(**k).is_some_and(|v| !v.is_null()))
        .count();
    if count != 1 {
        return Err("Each attachment must contain exactly one of url, path, or content".into());
    }
    let explicit_name = obj_str(&spec, "filename")?;
    let explicit_mime = obj_str(&spec, "mime_type")?;
    let cid = obj_str(&spec, "content_id")?
        .map(|v| content_id(&v))
        .transpose()?;
    let (bytes, name, mime) = if let Some(url) = obj_str(&spec, "url")? {
        match local_url(&url, owner)? {
            Some(v) => v,
            None => download(&url).await?,
        }
    } else if let Some(path) = obj_str(&spec, "path")? {
        let p = safe_path(&path)?;
        let meta = fs::metadata(&p).map_err(|_| "Attachment path is unavailable")?;
        if !meta.is_file() || meta.len() > MAX_ATTACHMENT as u64 {
            return Err(format!("Attachment exceeds {MAX_ATTACHMENT} bytes"));
        }
        let bytes = fs::read(&p).map_err(|_| "Failed to read attachment")?;
        let name = p
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("attachment")
            .to_owned();
        let mime = guess_mime(&name);
        (bytes, name, mime)
    } else {
        let encoded = obj_str(&spec, "content")?.ok_or("Attachment content must be a string")?;
        let bytes = b64decode(&encoded, false, MAX_ATTACHMENT)?;
        let name = explicit_name
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or("filename is required with attachment content")?;
        let mime = guess_mime(&name);
        (bytes, name, mime)
    };
    if bytes.len() > MAX_ATTACHMENT {
        return Err(format!("Attachment exceeds {MAX_ATTACHMENT} bytes"));
    }
    let name = sanitize(explicit_name.as_deref().unwrap_or(&name));
    let mime = explicit_mime.unwrap_or(mime);
    validate_mime(&mime)?;
    Ok(Resolved {
        bytes,
        filename: name,
        mime,
        cid,
    })
}
fn obj_str(o: &Map<String, Value>, k: &str) -> Result<Option<String>, String> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        _ => Err(format!("attachment.{k} must be a string")),
    }
}
fn validate_mime(v: &str) -> Result<(), String> {
    no_ctl(v, "mime_type")?;
    if !v.contains('/')
        || v.chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | ';'))
    {
        Err("mime_type must be a safe type/subtype value".into())
    } else {
        Ok(())
    }
}
fn guess_mime(name: &str) -> String {
    match Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
    .into()
}

async fn download(value: &str) -> Result<(Vec<u8>, String, String), String> {
    let mut current = Url::parse(value).map_err(|_| "Attachment URL is invalid")?;
    for redirect in 0..=10 {
        if !matches!(current.scheme(), "http" | "https")
            || current.host_str().is_none()
            || !current.username().is_empty()
            || current.password().is_some()
        {
            return Err(
                "Attachment URL must be an absolute http(s) URL without credentials".into(),
            );
        }
        let host = current.host_str().unwrap_or_default().to_owned();
        let port = current.port_or_known_default().unwrap_or(443);
        let addresses = public_addresses(&host, port).await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(&host, &addresses)
            .build()
            .map_err(|_| "Failed to initialize safe attachment client")?;
        let mut response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|_| "Failed to fetch attachment URL")?;
        if response.status().is_redirection() {
            if redirect == 10 {
                return Err("Attachment URL exceeded redirect limit".into());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("Attachment redirect has no valid Location")?;
            current = current
                .join(location)
                .map_err(|_| "Attachment redirect URL is invalid")?;
            continue;
        }
        if response.status() != StatusCode::OK {
            return Err(format!(
                "Failed to fetch attachment URL (status {})",
                response.status()
            ));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|v| v > MAX_ATTACHMENT)
        {
            return Err(format!("Attachment exceeds {MAX_ATTACHMENT} bytes"));
        }
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_owned());
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Failed to read attachment URL response")?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_ATTACHMENT {
                return Err(format!("Attachment exceeds {MAX_ATTACHMENT} bytes"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let name = current
            .path_segments()
            .and_then(Iterator::last)
            .filter(|v| !v.is_empty())
            .map(percent_decode)
            .unwrap_or_else(|| "attachment".into());
        let mime = mime
            .filter(|v| v != "application/octet-stream")
            .unwrap_or_else(|| guess_mime(&name));
        return Ok((bytes.to_vec(), name, mime));
    }
    Err("Attachment URL could not be fetched".into())
}
async fn public_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let h = host.to_owned();
    let v = tokio::task::spawn_blocking(move || {
        (h.as_str(), port)
            .to_socket_addrs()
            .map(|i| i.collect::<Vec<_>>())
    })
    .await
    .map_err(|_| "DNS resolution failed")?
    .map_err(|_| "DNS resolution failed")?;
    if v.is_empty() || v.iter().any(|a| !public_ip(a.ip())) {
        Err("URLs pointing to private/internal networks are not allowed".into())
    } else {
        Ok(v)
    }
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => public_v4(v),
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let s = v.segments();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || s[0] == 0x2002
                || (s[0] == 0x0064 && s[1] == 0xff9b)
                || (s[0] == 0x2001 && s[1] <= 0x01ff))
        }
    }
}
fn public_v4(ip: Ipv4Addr) -> bool {
    let v = u32::from(ip);
    ![
        ("0.0.0.0", 8),
        ("10.0.0.0", 8),
        ("100.64.0.0", 10),
        ("127.0.0.0", 8),
        ("169.254.0.0", 16),
        ("172.16.0.0", 12),
        ("192.0.0.0", 24),
        ("192.0.2.0", 24),
        ("192.88.99.0", 24),
        ("192.168.0.0", 16),
        ("198.18.0.0", 15),
        ("198.51.100.0", 24),
        ("203.0.113.0", 24),
        ("224.0.0.0", 4),
        ("240.0.0.0", 4),
    ]
    .iter()
    .any(|(n, p)| {
        let n = u32::from(n.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::UNSPECIFIED));
        let mask = u32::MAX << (32 - p);
        v & mask == n & mask
    })
}
fn percent_decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut o = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            o.push(h << 4 | l);
            i += 3;
            continue;
        }
        o.push(b[i]);
        i += 1
    }
    String::from_utf8_lossy(&o).into_owned()
}

fn local_url(value: &str, owner: &str) -> Result<Option<(Vec<u8>, String, String)>, String> {
    let url = Url::parse(value).map_err(|_| "Attachment URL is invalid")?;
    let origin = format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port().map(|p| format!(":{p}")).unwrap_or_default()
    );
    let parts: Vec<_> = url.path_segments().map(|v| v.collect()).unwrap_or_default();
    if parts.len() != 2 || parts[0] != "attachments" || !trusted_origins().contains(&origin) {
        return Ok(None);
    }
    let id = parts[1];
    if id.len() != 56 || id.chars().any(|c| !c.is_ascii_hexdigit()) {
        return Err("Invalid local attachment ID".into());
    }
    let dir = storage_root()?
        .join(segment(owner))
        .canonicalize()
        .map_err(|_| "Local attachment is unavailable")?;
    let mut matches = Vec::new();
    for e in fs::read_dir(&dir).map_err(|_| "Local attachment is unavailable")? {
        let e = e.map_err(|_| "Local attachment is unavailable")?;
        let name = e.file_name().to_string_lossy().into_owned();
        let exact_id = Path::new(&name)
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|stem| stem.ends_with(&format!("_{id}")));
        if exact_id && e.path().is_file() {
            matches.push(e.path())
        }
    }
    if matches.len() != 1 {
        return Err("Local attachment is unavailable".into());
    }
    let p = &matches[0];
    let m = fs::metadata(p).map_err(|_| "Local attachment is unavailable")?;
    if m.len() > MAX_ATTACHMENT as u64 {
        return Err(format!("Attachment exceeds {MAX_ATTACHMENT} bytes"));
    }
    let bytes = fs::read(p).map_err(|_| "Local attachment is unavailable")?;
    let name = p
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("attachment")
        .to_owned();
    Ok(Some((bytes, name.clone(), guess_mime(&name))))
}
fn trusted_origins() -> HashSet<String> {
    let mut o = HashSet::new();
    for v in [
        env::var("WORKSPACE_EXTERNAL_URL").ok(),
        Some(format!(
            "{}:{}",
            env::var("WORKSPACE_MCP_BASE_URI").unwrap_or_else(|_| "http://localhost".into()),
            env::var("WORKSPACE_MCP_PORT")
                .or_else(|_| env::var("PORT"))
                .unwrap_or_else(|_| "8000".into())
        )),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(u) = Url::parse(v.trim_end_matches('/'))
            && let Some(h) = u.host_str()
        {
            o.insert(format!(
                "{}://{}{}",
                u.scheme(),
                h.to_ascii_lowercase(),
                u.port().map(|p| format!(":{p}")).unwrap_or_default()
            ));
        }
    }
    o
}

fn safe_path(value: &str) -> Result<PathBuf, String> {
    if value.contains('\0') {
        return Err("Attachment path is not allowed".into());
    }
    let p = expand_home(PathBuf::from(value));
    let lower = p.to_string_lossy().to_ascii_lowercase();
    let parts: Vec<_> = p
        .components()
        .map(|v| v.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let name = p
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let sensitive = [
        ".credentials",
        ".credentials.json",
        "credentials.json",
        "client_secret.json",
        "client_secrets.json",
        "service_account.json",
        "service-account.json",
        ".npmrc",
        ".pypirc",
        ".netrc",
        ".git-credentials",
    ]
    .contains(&name.as_str());
    if parts.iter().any(|v| v == ".env" || v.starts_with(".env."))
        || parts
            .iter()
            .any(|v| matches!(v.as_str(), ".ssh" | ".aws" | ".kube" | ".gnupg"))
        || lower.contains("/.config/gcloud/")
        || ["/proc", "/sys", "/dev", "/etc/passwd", "/etc/shadow"]
            .iter()
            .any(|x| lower == *x || lower.starts_with(&format!("{x}/")))
        || sensitive
    {
        return Err("Attachment path is not allowed".into());
    }
    let allowed = allowed_dirs();
    if !allowed.iter().any(|r| p.is_absolute() && p.starts_with(r)) {
        return Err("Attachment path is outside permitted directories".into());
    }
    let resolved = p
        .canonicalize()
        .map_err(|_| "Attachment path is unavailable")?;
    if allowed.iter().any(|r| resolved.starts_with(r)) {
        Ok(resolved)
    } else {
        Err("Attachment path is outside permitted directories".into())
    }
}
fn allowed_dirs() -> Vec<PathBuf> {
    let mut o = Vec::new();
    if let Ok(root) = storage_root() {
        o.push(root.canonicalize().unwrap_or(root))
    }
    if let Some(v) = env::var_os("ALLOWED_FILE_DIRS") {
        for p in env::split_paths(&v) {
            if let Ok(p) = expand_home(p).canonicalize()
                && !o.contains(&p)
            {
                o.push(p)
            }
        }
    }
    o
}
struct Saved {
    id: String,
    name: String,
    path: PathBuf,
}
fn save(owner: &str, filename: &str, bytes: &[u8]) -> Result<Saved, String> {
    if bytes.len() > MAX_ATTACHMENT {
        return Err(format!("Attachment exceeds {MAX_ATTACHMENT} bytes"));
    }
    let owner = owner.trim().to_ascii_lowercase();
    if owner.is_empty() {
        return Err("An attachment owner is required".into());
    }
    let root = storage_root()?;
    private_dir(&root)?;
    let root = root
        .canonicalize()
        .map_err(|_| "Failed to resolve attachment storage directory")?;
    sweep(&root);
    let dir = root.join(segment(&owner));
    private_dir(&dir)?;
    let dir = dir
        .canonicalize()
        .map_err(|_| "Failed to resolve attachment owner directory")?;
    if !dir.starts_with(&root) {
        return Err("Attachment owner directory escaped storage root".into());
    }
    for _ in 0..16 {
        let id = unique_id();
        let safe = sanitize(filename);
        let p = Path::new(&safe);
        let stem = p
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("attachment");
        let name = match p.extension().and_then(|v| v.to_str()) {
            Some(e) if !e.is_empty() => format!("{stem}_{id}.{e}"),
            _ => format!("{stem}_{id}"),
        };
        let path = dir.join(&name);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&path) {
            Ok(mut f) => {
                f.write_all(bytes)
                    .and_then(|_| f.flush())
                    .and_then(|_| f.sync_all())
                    .map_err(|_| "Failed to persist attachment")?;
                secure_file(&path)?;
                return Ok(Saved { id, name, path });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("Failed to create attachment file".into()),
        }
    }
    Err("Failed to allocate attachment filename".into())
}
fn storage_root() -> Result<PathBuf, String> {
    if let Some(v) = env::var_os("WORKSPACE_ATTACHMENT_DIR") {
        return Ok(expand_home(v.into()));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or("Could not determine home directory")?;
    Ok(home.join(".workspace-mcp/attachments"))
}
fn private_dir(p: &Path) -> Result<(), String> {
    fs::create_dir_all(p).map_err(|_| "Failed to create attachment directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Failed to secure attachment directory")?;
    }
    Ok(())
}
fn secure_file(p: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o600))
            .map_err(|_| "Failed to secure attachment file")?;
    }
    Ok(())
}
fn sweep(root: &Path) {
    let Ok(owners) = fs::read_dir(root) else {
        return;
    };
    for owner in owners.flatten() {
        let Ok(entries) = fs::read_dir(owner.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let expired = e
                .metadata()
                .ok()
                .filter(|v| v.is_file())
                .and_then(|v| v.modified().ok())
                .and_then(|v| v.elapsed().ok())
                .is_some_and(|v| v.as_secs() >= EXPIRY);
            if expired {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}
fn sanitize(v: &str) -> String {
    let mut o = String::new();
    for c in v.chars().take(180) {
        if c.is_control() || "<>:\"/\\|?*".contains(c) {
            o.push('_')
        } else if c.is_whitespace() && !c.is_ascii_whitespace() {
            o.push(' ')
        } else {
            o.push(c)
        }
    }
    while o.ends_with([' ', '.']) {
        o.pop();
    }
    if o.is_empty() {
        o = "attachment".into()
    }
    let stem = o.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        o.insert(0, '_')
    }
    o
}
fn unique_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{now:032x}{:08x}{seq:016x}", std::process::id())
}
fn attachment_url(id: &str) -> Result<String, String> {
    let base = env::var("WORKSPACE_EXTERNAL_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}:{}",
                env::var("WORKSPACE_MCP_BASE_URI").unwrap_or_else(|_| "http://localhost".into()),
                env::var("WORKSPACE_MCP_PORT")
                    .or_else(|_| env::var("PORT"))
                    .unwrap_or_else(|_| "8000".into())
            )
        });
    let u = Url::parse(&base).map_err(|_| "Cannot build attachment URL")?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("Cannot build attachment URL".into());
    }
    Ok(format!("{}/attachments/{id}", base.trim_end_matches('/')))
}
fn stateless() -> bool {
    env::var("WORKSPACE_MCP_STATELESS_MODE").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
}
fn transport() -> String {
    env::var("WORKSPACE_MCP_TRANSPORT").unwrap_or_else(|_| "stdio".into())
}
fn expand_home(p: PathBuf) -> PathBuf {
    let Some(v) = p.to_str() else { return p };
    if v == "~" {
        return env::var_os("HOME").map(PathBuf::from).unwrap_or(p);
    }
    if let Some(rest) = v.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    p
}
fn segment(v: &str) -> String {
    utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()
}

async fn export(
    session: &GoogleSession,
    h: &HashMap<String, String>,
    bytes: Vec<u8>,
    ext: &str,
    _mime: &str,
    note: Option<&str>,
) -> Result<String, String> {
    let mut o = header_lines(h, None);
    o.push(if stateless() {
        "\n--- FULL MESSAGE ---".into()
    } else {
        "\n--- FULL MESSAGE EXPORT ---".into()
    });
    o.push(format!("Format: {}", ext.trim_start_matches('.')));
    o.push(format!(
        "Size: {:.1} KB ({} bytes)",
        bytes.len() as f64 / 1024.0,
        bytes.len()
    ));
    if let Some(n) = note {
        o.push(format!("Note: {n}"))
    }
    if stateless() {
        o.extend([
            "\nStateless mode: complete untruncated content follows.".into(),
            format!(
                "\n--- BODY (COMPLETE, NOT TRUNCATED) ---\n{}",
                String::from_utf8_lossy(&bytes)
            ),
        ])
    } else {
        let name = format!(
            "{}{}",
            hv(h, "Subject", "message")
                .chars()
                .take(80)
                .collect::<String>(),
            ext
        );
        let saved = save(&session.user_email, &name, &bytes)?;
        o.push(format!("Saved filename: {}", saved.name));
        if transport() == "stdio" {
            o.push(format!("\n📎 Saved to: {}", saved.path.display()))
        } else {
            o.push(format!("\n📎 Download URL: {}", attachment_url(&saved.id)?))
        }
    }
    Ok(o.join("\n"))
}
async fn attachment_meta(
    session: &GoogleSession,
    mid: &str,
    aid: &str,
    size: u64,
) -> Option<(String, String)> {
    let v = fetch_message(session, mid, "full").await.ok()?;
    let at = attachment_metas(v.get("payload")?);
    at.iter()
        .find(|x| x.id == aid)
        .or_else(|| {
            let m: Vec<_> = at.iter().filter(|x| x.size.abs_diff(size) < 100).collect();
            (m.len() == 1).then(|| m[0])
        })
        .or_else(|| (at.len() == 1).then(|| &at[0]))
        .map(|x| (x.filename.clone(), x.mime.clone()))
}
fn attachment_result(a: usize, r: usize) -> String {
    if r == 0 {
        String::new()
    } else if a == r {
        format!(" with {a} attachment(s)")
    } else {
        format!(" with {a}/{r} attachment(s) attached")
    }
}
fn label_actions(a: &[String], r: &[String]) -> String {
    let mut o = Vec::new();
    if !a.is_empty() {
        o.push(format!("Added labels: {}", a.join(", ")))
    }
    if !r.is_empty() {
        o.push(format!("Removed labels: {}", r.join(", ")))
    }
    o.join("; ")
}

fn b64encode(bytes: &[u8], url: bool) -> String {
    let alphabet = if url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut o = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for x in bytes.chunks(3) {
        let a = x[0];
        let b = x.get(1).copied().unwrap_or(0);
        let c = x.get(2).copied().unwrap_or(0);
        o.push(alphabet[(a >> 2) as usize] as char);
        o.push(alphabet[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        o.push(if x.len() > 1 {
            alphabet[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        o.push(if x.len() > 2 {
            alphabet[(c & 63) as usize] as char
        } else {
            '='
        })
    }
    o
}
fn b64decode(value: &str, url: bool, limit: usize) -> Result<Vec<u8>, String> {
    let compact: Vec<u8> = value.bytes().filter(|v| !v.is_ascii_whitespace()).collect();
    if limit != usize::MAX && compact.len() > limit.div_ceil(3) * 4 {
        return Err(format!("Attachment exceeds {limit} bytes"));
    }
    let first_padding = compact.iter().position(|byte| *byte == b'=');
    if let Some(index) = first_padding {
        let padding = compact.len() - index;
        if padding > 2
            || !compact.len().is_multiple_of(4)
            || compact[index..].iter().any(|byte| *byte != b'=')
        {
            return Err("Invalid base64 padding".into());
        }
    }
    let data_len = first_padding.unwrap_or(compact.len());
    if data_len % 4 == 1 {
        return Err("Invalid base64 length".into());
    }
    let mut out = Vec::with_capacity(compact.len().div_ceil(4) * 3);
    for (group_index, group) in compact.chunks(4).enumerate() {
        let mut chunk = [b'='; 4];
        chunk[..group.len()].copy_from_slice(group);
        if (chunk[0] == b'=' || chunk[1] == b'=')
            || (group_index + 1 < compact.len().div_ceil(4) && chunk.contains(&b'='))
        {
            return Err("Invalid base64 padding".into());
        }
        let mut n = [0_u8; 4];
        for index in 0..4 {
            n[index] = match chunk[index] {
                b'A'..=b'Z' => chunk[index] - b'A',
                b'a'..=b'z' => chunk[index] - b'a' + 26,
                b'0'..=b'9' => chunk[index] - b'0' + 52,
                b'+' if !url => 62,
                b'/' if !url => 63,
                b'-' if url => 62,
                b'_' if url => 63,
                b'=' => 0,
                _ => return Err("Invalid base64 data".into()),
            };
        }
        if chunk[2] == b'=' {
            if chunk[3] != b'=' || n[1] & 0x0f != 0 {
                return Err("Invalid base64 trailing bits".into());
            }
        } else if chunk[3] == b'=' && n[2] & 0x03 != 0 {
            return Err("Invalid base64 trailing bits".into());
        }
        out.push(n[0] << 2 | n[1] >> 4);
        if chunk[2] != b'=' {
            out.push(n[1] << 4 | n[2] >> 2);
        }
        if chunk[3] != b'=' {
            out.push(n[2] << 6 | n[3]);
        }
    }
    if out.len() > limit {
        return Err(format!("Attachment exceeds {limit} bytes"));
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    OptStr,
    Int,
    Bool,
    List,
    DictList,
    Dict,
}
fn kind(n: &str) -> Kind {
    match n {
        "page_size" => Kind::Int,
        "include_headers"
        | "full"
        | "return_base64"
        | "include_forwarded_attachments"
        | "include_signature"
        | "quote_original"
        | "reply_all"
        | "include_analysis" => Kind::Bool,
        "message_ids" | "thread_ids" | "add_label_ids" | "remove_label_ids" => Kind::List,
        "attachments" => Kind::DictList,
        "criteria" | "filter_action" => Kind::Dict,
        "page_token" | "to" | "subject" | "body" | "forward_message_id" | "cc" | "bcc"
        | "from_name" | "from_email" | "thread_id" | "in_reply_to" | "references" | "name"
        | "label_id" | "filter_id" => Kind::OptStr,
        _ => Kind::Str,
    }
}
fn validate(a: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    for (k, v) in a {
        let Some(c) = allowed.iter().find(|n| k == **n || k == &camel(n)) else {
            return Err(format!("Unexpected argument: {k}"));
        };
        let alias = camel(c);
        if k == &alias && alias != **c && a.contains_key(*c) {
            return Err(format!("Unexpected argument: {k}"));
        }
        let valid = match kind(c) {
            Kind::Str => v.is_string(),
            Kind::OptStr => v.is_null() || v.is_string(),
            Kind::Int => v.as_i64().is_some(),
            Kind::Bool => v.is_boolean(),
            Kind::List | Kind::DictList => v.is_null() || v.is_array() || v.is_string(),
            Kind::Dict => v.is_null() || v.is_object() || v.is_string(),
        };
        if !valid {
            return Err(format!("{c} has an invalid type"));
        }
    }
    Ok(())
}
fn arg<'a>(a: &'a Map<String, Value>, n: &str) -> Option<&'a Value> {
    a.get(n).or_else(|| a.get(&camel(n)))
}
fn req_str(a: &Map<String, Value>, n: &str) -> Result<String, String> {
    opt_str(a, n)?.ok_or_else(|| format!("{n} is required"))
}
fn opt_str(a: &Map<String, Value>, n: &str) -> Result<Option<String>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        _ => Err(format!("{n} must be a string")),
    }
}
fn int(a: &Map<String, Value>, n: &str, d: i64) -> Result<i64, String> {
    match arg(a, n) {
        None => Ok(d),
        Some(v) => v.as_i64().ok_or_else(|| format!("{n} must be an integer")),
    }
}
fn bool_arg(a: &Map<String, Value>, n: &str, d: bool) -> Result<bool, String> {
    match arg(a, n) {
        None => Ok(d),
        Some(v) => v.as_bool().ok_or_else(|| format!("{n} must be a boolean")),
    }
}
fn enum_arg(a: &Map<String, Value>, n: &str, d: &str, allowed: &[&str]) -> Result<String, String> {
    let v = match arg(a, n) {
        None => d.into(),
        Some(Value::String(v)) => v.clone(),
        _ => return Err(format!("{n} must be a string")),
    };
    if allowed.contains(&v.as_str()) {
        Ok(v)
    } else {
        Err(format!("{n} must be one of: {}", allowed.join(", ")))
    }
}
fn str_list(
    a: &Map<String, Value>,
    n: &str,
    nullable: bool,
) -> Result<Option<Vec<String>>, String> {
    let Some(v) = arg(a, n) else { return Ok(None) };
    if v.is_null() && nullable {
        return Ok(None);
    }
    let parsed;
    let array = if let Some(x) = v.as_array() {
        x
    } else if let Some(x) = v.as_str() {
        parsed = serde_json::from_str::<Value>(x)
            .map_err(|_| format!("{n} must be an array or JSON array string"))?;
        parsed
            .as_array()
            .ok_or_else(|| format!("{n} must be an array or JSON array string"))?
    } else {
        return Err(format!("{n} must be an array or JSON array string"));
    };
    array
        .iter()
        .map(|x| {
            x.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{n} entries must be strings"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}
fn dict_list(a: &Map<String, Value>, n: &str) -> Result<Option<Vec<Map<String, Value>>>, String> {
    let Some(v) = arg(a, n) else { return Ok(None) };
    if v.is_null() {
        return Ok(None);
    }
    let parsed;
    let array = if let Some(x) = v.as_array() {
        x
    } else if let Some(x) = v.as_str() {
        parsed = serde_json::from_str::<Value>(x)
            .map_err(|_| format!("{n} must be an array or JSON array string"))?;
        parsed
            .as_array()
            .ok_or_else(|| format!("{n} must be an array or JSON array string"))?
    } else {
        return Err(format!("{n} must be an array or JSON array string"));
    };
    array
        .iter()
        .map(|x| {
            x.as_object()
                .cloned()
                .ok_or_else(|| format!("{n} entries must be objects"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}
fn dict(a: &Map<String, Value>, n: &str) -> Result<Option<Map<String, Value>>, String> {
    let Some(v) = arg(a, n) else { return Ok(None) };
    if v.is_null() {
        return Ok(None);
    }
    if let Some(o) = v.as_object() {
        return Ok(Some(o.clone()));
    }
    if let Some(x) = v.as_str() {
        return serde_json::from_str::<Value>(x)
            .map_err(|_| format!("{n} must be an object or JSON object string"))?
            .as_object()
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{n} must be an object or JSON object string"));
    }
    Err(format!("{n} must be an object or JSON object string"))
}
fn camel(v: &str) -> String {
    let mut o = String::new();
    let mut upper = false;
    for c in v.chars() {
        if c == '_' {
            upper = true
        } else if upper {
            o.extend(c.to_uppercase());
            upper = false
        } else {
            o.push(c)
        }
    }
    o
}
fn hex(v: u8) -> Option<u8> {
    match v {
        b'0'..=b'9' => Some(v - b'0'),
        b'a'..=b'f' => Some(v - b'a' + 10),
        b'A'..=b'F' => Some(v - b'A' + 10),
        _ => None,
    }
}
