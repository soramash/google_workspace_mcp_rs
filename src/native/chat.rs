use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use reqwest::header::CONTENT_LENGTH;
use serde_json::{Map, Value, json};

use crate::auth::GoogleSession;

const CHAT_API: &str = "https://chat.googleapis.com/v1";
const PEOPLE_API: &str = "https://people.googleapis.com/v1";
const CHAT_SPACES_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/chat.spaces.readonly";
const CHAT_SPACES_SCOPE: &str = "https://www.googleapis.com/auth/chat.spaces";
const CHAT_MESSAGES_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/chat.messages.readonly";
const CHAT_MESSAGES_SCOPE: &str = "https://www.googleapis.com/auth/chat.messages";
const CONTACTS_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/contacts.readonly";
const CONTACTS_SCOPE: &str = "https://www.googleapis.com/auth/contacts";
const SPACES_READ_SCOPE_SETS: &[&[&str]] = &[&[CHAT_SPACES_READONLY_SCOPE], &[CHAT_SPACES_SCOPE]];
const MESSAGE_READ_SCOPE_SETS: &[&[&str]] =
    &[&[CHAT_MESSAGES_READONLY_SCOPE], &[CHAT_MESSAGES_SCOPE]];
const MESSAGE_AND_CONTACTS_READ_SCOPE_SETS: &[&[&str]] = &[
    &[CHAT_MESSAGES_READONLY_SCOPE, CONTACTS_READONLY_SCOPE],
    &[CHAT_MESSAGES_SCOPE, CONTACTS_READONLY_SCOPE],
    &[CHAT_MESSAGES_READONLY_SCOPE, CONTACTS_SCOPE],
    &[CHAT_MESSAGES_SCOPE, CONTACTS_SCOPE],
];
const MESSAGE_WRITE_SCOPE_SETS: &[&[&str]] = &[&[CHAT_MESSAGES_SCOPE]];
const MAX_CHAT_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;
const ATTACHMENT_EXPIRATION_SECONDS: u64 = 3600;
const SENDER_CACHE_CAPACITY: usize = 256;
const STATELESS_PREVIEW_INPUT_BYTES: usize = 75;

const LIST_SPACES_ARGUMENTS: &[&str] = &["user_google_email", "page_size", "space_type"];
const GET_MESSAGES_ARGUMENTS: &[&str] = &[
    "user_google_email",
    "space_id",
    "page_size",
    "order_by",
    "message_filter",
];
const SEND_MESSAGE_ARGUMENTS: &[&str] = &[
    "user_google_email",
    "space_id",
    "message_text",
    "thread_key",
    "thread_name",
];
const SEARCH_MESSAGES_ARGUMENTS: &[&str] = &[
    "user_google_email",
    "query",
    "space_id",
    "page_size",
    "time_filter",
    "max_spaces",
];
const CREATE_REACTION_ARGUMENTS: &[&str] = &["user_google_email", "message_id", "emoji_unicode"];
const DOWNLOAD_ATTACHMENT_ARGUMENTS: &[&str] =
    &["user_google_email", "message_id", "attachment_index"];

static SENDER_CACHE: OnceLock<Mutex<SenderCache>> = OnceLock::new();
static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub async fn dispatch(tool_name: &str, arguments: &Map<String, Value>) -> Result<String, String> {
    match tool_name {
        "send_message" => send_message(arguments).await,
        "get_messages" => get_messages(arguments).await,
        "search_messages" => search_messages(arguments).await,
        "create_reaction" => create_reaction(arguments).await,
        "list_spaces" => list_spaces(arguments).await,
        "download_chat_attachment" => download_chat_attachment(arguments).await,
        _ => Err(format!("Unsupported native Chat tool: {tool_name}")),
    }
}

async fn list_spaces(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, LIST_SPACES_ARGUMENTS)?;
    let page_size = integer(arguments, "page_size", 100)?;
    let space_type = string_with_default(arguments, "space_type", "all")?;
    let session = authenticated_session(arguments, SPACES_READ_SCOPE_SETS).await?;

    let mut query = vec![(String::from("pageSize"), page_size.to_string())];
    match space_type.as_str() {
        "room" => query.push((String::from("filter"), String::from("spaceType = SPACE"))),
        "dm" => query.push((
            String::from("filter"),
            String::from("spaceType = DIRECT_MESSAGE"),
        )),
        _ => {}
    }
    let result = send(
        &session,
        "list_spaces",
        Method::GET,
        &format!("{CHAT_API}/spaces"),
        &query,
        None,
    )
    .await?;
    let spaces = array(&result, "spaces");
    if spaces.is_empty() {
        return Ok(format!("No Chat spaces found for type '{space_type}'."));
    }

    let mut output = vec![format!(
        "Found {} Chat spaces (type: {space_type}):",
        spaces.len()
    )];
    for space in spaces {
        output.push(format!(
            "- {} (ID: {}, Type: {})",
            api_string(space, "displayName").unwrap_or("Unnamed Space"),
            api_string(space, "name").unwrap_or(""),
            api_string(space, "spaceType").unwrap_or("UNKNOWN")
        ));
    }
    Ok(output.join("\n"))
}

async fn get_messages(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, GET_MESSAGES_ARGUMENTS)?;
    let space_id = required_string(arguments, "space_id")?;
    let encoded_space = validated_space(&space_id)?;
    let page_size = integer(arguments, "page_size", 50)?;
    let order_by = string_with_default(arguments, "order_by", "createTime desc")?;
    let message_filter = nullable_string(arguments, "message_filter")?;
    let session = authenticated_session(arguments, MESSAGE_AND_CONTACTS_READ_SCOPE_SETS).await?;

    let space = send(
        &session,
        "get_messages",
        Method::GET,
        &format!("{CHAT_API}/{encoded_space}"),
        &[],
        None,
    )
    .await?;
    let space_name = api_string(&space, "displayName").unwrap_or("Unknown Space");

    let mut query = vec![
        (String::from("pageSize"), page_size.to_string()),
        (String::from("orderBy"), order_by),
    ];
    if let Some(message_filter) = message_filter {
        query.push((String::from("filter"), message_filter));
    }
    let result = send(
        &session,
        "get_messages",
        Method::GET,
        &format!("{CHAT_API}/{encoded_space}/messages"),
        &query,
        None,
    )
    .await?;
    let messages = array(&result, "messages");
    if messages.is_empty() {
        return Ok(format!(
            "No messages found in space '{space_name}' (ID: {space_id})."
        ));
    }

    let sender_names = resolve_senders(&session, messages).await;
    let mut output = vec![format!("Messages from '{space_name}' (ID: {space_id}):\n")];
    for message in messages {
        append_detailed_message(&mut output, message, &sender_names, &session).await;
    }
    Ok(output.join("\n"))
}

async fn send_message(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, SEND_MESSAGE_ARGUMENTS)?;
    let space_id = required_string(arguments, "space_id")?;
    let encoded_space = validated_space(&space_id)?;
    let message_text = required_string(arguments, "message_text")?;
    let thread_key = nullable_string(arguments, "thread_key")?;
    let thread_name = nullable_string(arguments, "thread_name")?;
    let session = authenticated_session(arguments, MESSAGE_WRITE_SCOPE_SETS).await?;

    let mut body = json!({"text": message_text});
    let mut query = Vec::new();
    if let Some(thread_name) = thread_name.filter(|value| !value.is_empty()) {
        validated_thread(&thread_name)?;
        body["thread"] = json!({"name": thread_name});
        query.push((
            String::from("messageReplyOption"),
            String::from("REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD"),
        ));
    } else if let Some(thread_key) = thread_key.filter(|value| !value.is_empty()) {
        body["thread"] = json!({"threadKey": thread_key});
        query.push((
            String::from("messageReplyOption"),
            String::from("REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD"),
        ));
    }

    let result = send(
        &session,
        "send_message",
        Method::POST,
        &format!("{CHAT_API}/{encoded_space}/messages"),
        &query,
        Some(&body),
    )
    .await?;
    Ok(format!(
        "Message sent to space '{space_id}' by {}. Message ID: {}, Time: {}",
        session.user_email,
        api_string(&result, "name").unwrap_or(""),
        api_string(&result, "createTime").unwrap_or("")
    ))
}

async fn search_messages(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, SEARCH_MESSAGES_ARGUMENTS)?;
    let query_text = nullable_string(arguments, "query")?;
    let space_id = nullable_string(arguments, "space_id")?;
    let page_size = integer(arguments, "page_size", 25)?;
    let time_filter = nullable_string(arguments, "time_filter")?;
    let max_spaces = integer(arguments, "max_spaces", 10)?;
    let session = authenticated_session(arguments, MESSAGE_AND_CONTACTS_READ_SCOPE_SETS).await?;

    let mut search_terms = Vec::new();
    if let Some(query) = query_text.as_deref().filter(|value| !value.is_empty()) {
        search_terms.push(format!("text \"{query}\""));
    }
    if let Some(time_filter) = time_filter.as_deref().filter(|value| !value.is_empty()) {
        search_terms.push(time_filter.to_owned());
    }
    let search_description = if search_terms.is_empty() {
        String::from("all messages")
    } else {
        search_terms.join(" and ")
    };

    let mut hits = Vec::new();
    let mut transient_failures = 0usize;
    let context;
    if let Some(space_id) = space_id.as_deref().filter(|value| !value.is_empty()) {
        let encoded_space = validated_space(space_id)?;
        let result =
            list_messages_for_search(&session, &encoded_space, page_size, time_filter.as_deref())
                .await?;
        hits.extend(
            array(&result, "messages")
                .iter()
                .cloned()
                .map(|message| SearchHit {
                    message,
                    space_name: String::from("Unknown Space"),
                }),
        );
        context = format!("space '{space_id}'");
    } else {
        let spaces_result = send(
            &session,
            "search_messages",
            Method::GET,
            &format!("{CHAT_API}/spaces"),
            &[(String::from("pageSize"), String::from("100"))],
            None,
        )
        .await?;
        let spaces = array(&spaces_result, "spaces");
        let end = python_slice_end(spaces.len(), max_spaces);
        let spaces_to_search = &spaces[..end];
        for space in spaces_to_search {
            let Some(space_resource) = api_string(space, "name") else {
                continue;
            };
            let Ok(encoded_space) = validated_space(space_resource) else {
                continue;
            };
            match list_messages_for_search(
                &session,
                &encoded_space,
                page_size,
                time_filter.as_deref(),
            )
            .await
            {
                Ok(result) => {
                    let display_name = api_string(space, "displayName").unwrap_or("Unknown");
                    hits.extend(array(&result, "messages").iter().cloned().map(|message| {
                        SearchHit {
                            message,
                            space_name: display_name.to_owned(),
                        }
                    }));
                }
                Err(error) if is_transient_network_error(&error) => transient_failures += 1,
                Err(_) => {}
            }
        }
        if !spaces_to_search.is_empty() && transient_failures == spaces_to_search.len() {
            return Err(String::from(
                "A transient network error occurred in 'search_messages' while searching Chat spaces. Please try again shortly.",
            ));
        }
        context = String::from("all accessible spaces");
    }

    if let Some(query) = query_text.as_deref().filter(|value| !value.is_empty()) {
        let query = query.to_lowercase();
        hits.retain(|hit| {
            api_string(&hit.message, "text")
                .unwrap_or("")
                .to_lowercase()
                .contains(&query)
        });
    }
    if hits.is_empty() {
        let suffix = if transient_failures > 0 {
            format!(" Skipped {transient_failures} spaces due to repeated network failures.")
        } else {
            String::new()
        };
        return Ok(format!(
            "No messages found matching '{search_description}' in {context}.{suffix}"
        ));
    }

    let message_values: Vec<Value> = hits.iter().map(|hit| hit.message.clone()).collect();
    let sender_names = resolve_senders(&session, &message_values).await;
    let mut output = vec![format!(
        "Found {} messages matching '{search_description}' in {context}:",
        hits.len()
    )];
    for hit in &hits {
        let sender = sender_for_message(&hit.message, &sender_names, &session).await;
        let create_time = api_string(&hit.message, "createTime").unwrap_or("Unknown Time");
        let text = truncate_chars(
            api_string(&hit.message, "text").unwrap_or("No text content"),
            100,
        );
        let mut suffix = String::new();
        for rich_link in rich_links(&hit.message) {
            suffix.push_str(&format!(" [linked: {rich_link}]"));
        }
        for attachment in array(&hit.message, "attachment") {
            suffix.push_str(&format!(
                " [attachment: {} ({})]",
                api_string(attachment, "contentName").unwrap_or("unnamed"),
                api_string(attachment, "contentType").unwrap_or("unknown type")
            ));
        }
        output.push(format!(
            "- [{create_time}] {sender} in '{}': {text}{suffix}",
            hit.space_name
        ));
    }
    Ok(output.join("\n"))
}

async fn create_reaction(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, CREATE_REACTION_ARGUMENTS)?;
    let message_id = required_string(arguments, "message_id")?;
    let encoded_message = validated_message(&message_id)?;
    let emoji_unicode = required_string(arguments, "emoji_unicode")?;
    let session = authenticated_session(arguments, MESSAGE_WRITE_SCOPE_SETS).await?;
    let result = send(
        &session,
        "create_reaction",
        Method::POST,
        &format!("{CHAT_API}/{encoded_message}/reactions"),
        &[],
        Some(&json!({"emoji": {"unicode": emoji_unicode}})),
    )
    .await?;
    Ok(format!(
        "Reacted with {emoji_unicode} on message {message_id}. Reaction ID: {}",
        api_string(&result, "name").unwrap_or("")
    ))
}

async fn download_chat_attachment(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, DOWNLOAD_ATTACHMENT_ARGUMENTS)?;
    let message_id = required_string(arguments, "message_id")?;
    let encoded_message = validated_message(&message_id)?;
    let attachment_index = integer(arguments, "attachment_index", 0)?;
    let session = authenticated_session(arguments, MESSAGE_READ_SCOPE_SETS).await?;

    let message = send(
        &session,
        "download_chat_attachment",
        Method::GET,
        &format!("{CHAT_API}/{encoded_message}"),
        &[],
        None,
    )
    .await?;
    let attachments = array(&message, "attachment");
    if attachments.is_empty() {
        return Ok(format!("No attachments found on message {message_id}."));
    }
    let Ok(index) = usize::try_from(attachment_index) else {
        return Ok(invalid_attachment_index(
            attachment_index,
            attachments.len(),
        ));
    };
    let Some(attachment) = attachments.get(index) else {
        return Ok(invalid_attachment_index(
            attachment_index,
            attachments.len(),
        ));
    };

    let filename = api_string(attachment, "contentName").unwrap_or("attachment");
    let content_type = api_string(attachment, "contentType").unwrap_or("application/octet-stream");
    let media_resource = attachment
        .pointer("/attachmentDataRef/resourceName")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| api_string(attachment, "name").filter(|value| !value.is_empty()));
    let Some(media_resource) = media_resource else {
        return Ok(format!(
            "No resource name available for attachment '{filename}'."
        ));
    };
    let encoded_resource = validated_attachment(media_resource)?;
    let media_url = format!("{CHAT_API}/media/{encoded_resource}?alt=media");
    let mut response = session
        .request(Method::GET, &media_url)
        .send()
        .await
        .map_err(|error| format!("Failed to download attachment '{filename}': {error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = read_error_preview(&mut response).await;
        return Ok(format!(
            "Failed to download attachment '{filename}': HTTP {status} from {media_url}\n{body}"
        ));
    }
    if let Some(declared) = declared_content_length(&response)
        && declared > MAX_CHAT_ATTACHMENT_BYTES
    {
        return Ok(format!(
            "Attachment '{filename}' exceeds the {MAX_CHAT_ATTACHMENT_BYTES} byte limit (declared {declared} bytes)."
        ));
    }

    if is_stateless_mode() {
        let (size_bytes, preview) = match stream_stateless(&mut response, filename).await {
            Ok(result) => result,
            Err(error) => return Ok(error),
        };
        let size_kb = size_bytes as f64 / 1024.0;
        return Ok([
            format!("Attachment downloaded: {filename} ({content_type})"),
            format!("Size: {size_kb:.1} KB ({size_bytes} bytes)"),
            String::new(),
            String::from("Stateless mode: File storage disabled."),
            format!("Base64 preview: {preview}..."),
        ]
        .join("\n"));
    }

    let owner = normalize_owner(&session.user_email)?;
    let prepared = prepare_attachment_file(filename, content_type, &owner)?;
    let download_result = stream_to_file(&mut response, filename, prepared.file).await;
    let (size_bytes, mut file) = match download_result {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_file(&prepared.path);
            if error.starts_with("Attachment '")
                || error.starts_with("Failed to download attachment '")
            {
                return Ok(error);
            }
            return Err(error);
        }
    };
    if let Err(error) = file.flush().and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&prepared.path);
        return Err(format!(
            "Failed to save attachment '{filename}' to {}: {error}",
            prepared.path.display()
        ));
    }

    let size_kb = size_bytes as f64 / 1024.0;
    let mut output = vec![
        format!("Attachment downloaded: {filename}"),
        format!("Type: {content_type}"),
        format!("Size: {size_kb:.1} KB ({size_bytes} bytes)"),
    ];
    if transport_mode() == "stdio" {
        output.push(format!("\nSaved to: {}", prepared.path.display()));
        output.push(String::from(
            "\nThe file has been saved to disk and can be accessed directly via the file path.",
        ));
    } else {
        let url = match attachment_url(&prepared.file_id) {
            Ok(url) => url,
            Err(error) => {
                let _ = fs::remove_file(&prepared.path);
                return Err(error);
            }
        };
        output.push(format!("\nDownload URL: {url}"));
        output.push(String::from("\nThe file will expire after 1 hour."));
    }
    Ok(output.join("\n"))
}

async fn authenticated_session(
    arguments: &Map<String, Value>,
    required_scope_sets: &[&[&str]],
) -> Result<GoogleSession, String> {
    let supplied_email = required_string(arguments, "user_google_email")?;
    let mut first_error = None;
    for required_scopes in required_scope_sets {
        match GoogleSession::authenticate(required_scopes).await {
            Ok(session) => {
                if !supplied_email.eq_ignore_ascii_case(&session.user_email) {
                    return Err(String::from(
                        "user_google_email does not match the configured USER_GOOGLE_EMAIL",
                    ));
                }
                return Ok(session);
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| String::from("No Google scopes were configured")))
}

async fn send(
    session: &GoogleSession,
    tool_name: &str,
    method: Method,
    url: &str,
    query: &[(String, String)],
    body: Option<&Value>,
) -> Result<Value, String> {
    let mut request = session.request(method, url);
    if !query.is_empty() {
        request = request.query(query);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    session.send_json(tool_name, request).await
}

async fn list_messages_for_search(
    session: &GoogleSession,
    encoded_space: &str,
    page_size: i64,
    time_filter: Option<&str>,
) -> Result<Value, String> {
    let mut query = vec![(String::from("pageSize"), page_size.to_string())];
    if let Some(time_filter) = time_filter.filter(|value| !value.is_empty()) {
        query.push((String::from("filter"), time_filter.to_owned()));
    }
    send(
        session,
        "search_messages",
        Method::GET,
        &format!("{CHAT_API}/{encoded_space}/messages"),
        &query,
        None,
    )
    .await
}

async fn resolve_senders(session: &GoogleSession, messages: &[Value]) -> HashMap<String, String> {
    let mut unique = HashMap::new();
    for message in messages {
        let Some(sender) = message.get("sender") else {
            continue;
        };
        let Some(name) = api_string(sender, "name").filter(|value| !value.is_empty()) else {
            continue;
        };
        unique
            .entry(name.to_owned())
            .or_insert_with(|| sender.clone());
    }

    let mut resolved = HashMap::new();
    for (name, sender) in unique {
        let display_name = resolve_sender(session, &sender).await;
        resolved.insert(name, display_name);
    }
    resolved
}

async fn sender_for_message(
    message: &Value,
    sender_names: &HashMap<String, String>,
    session: &GoogleSession,
) -> String {
    let sender = message.get("sender").unwrap_or(&Value::Null);
    let key = api_string(sender, "name").unwrap_or("");
    if let Some(display_name) = sender_names.get(key) {
        return display_name.clone();
    }
    resolve_sender(session, sender).await
}

async fn resolve_sender(session: &GoogleSession, sender: &Value) -> String {
    if let Some(display_name) = api_string(sender, "displayName").filter(|value| !value.is_empty())
    {
        return display_name.to_owned();
    }
    let Some(user_resource) = api_string(sender, "name").filter(|value| !value.is_empty()) else {
        return String::from("Unknown Sender");
    };
    if let Some(cached) = sender_cache_get(user_resource) {
        return cached;
    }
    let Some(user_id) = user_resource.strip_prefix("users/") else {
        sender_cache_insert(user_resource, user_resource);
        return user_resource.to_owned();
    };
    if user_id.is_empty() || user_id.contains('/') || user_id.chars().any(char::is_control) {
        sender_cache_insert(user_resource, user_resource);
        return user_resource.to_owned();
    }

    let people_resource = format!("people/{}", path_segment(user_id));
    let query = [(
        String::from("personFields"),
        String::from("names,emailAddresses"),
    )];
    let resolved = match send(
        session,
        "get_messages",
        Method::GET,
        &format!("{PEOPLE_API}/{people_resource}"),
        &query,
        None,
    )
    .await
    {
        Ok(person) => array(&person, "names")
            .first()
            .and_then(|name| api_string(name, "displayName"))
            .filter(|value| !value.is_empty())
            .or_else(|| {
                array(&person, "emailAddresses")
                    .first()
                    .and_then(|email| api_string(email, "value"))
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or(user_resource)
            .to_owned(),
        Err(_) => user_resource.to_owned(),
    };
    sender_cache_insert(user_resource, &resolved);
    resolved
}

async fn append_detailed_message(
    output: &mut Vec<String>,
    message: &Value,
    sender_names: &HashMap<String, String>,
    session: &GoogleSession,
) {
    let sender = sender_for_message(message, sender_names, session).await;
    let create_time = api_string(message, "createTime").unwrap_or("Unknown Time");
    let text = api_string(message, "text").unwrap_or("No text content");
    let message_name = api_string(message, "name").unwrap_or("");
    output.push(format!("[{create_time}] {sender}:"));
    output.push(format!("  {text}"));
    for rich_link in rich_links(message) {
        output.push(format!("  [linked: {rich_link}]"));
    }
    for (index, attachment) in array(message, "attachment").iter().enumerate() {
        output.push(format!(
            "  [attachment {index}: {} ({})]",
            api_string(attachment, "contentName").unwrap_or("unnamed"),
            api_string(attachment, "contentType").unwrap_or("unknown type")
        ));
        if api_string(attachment, "name").is_some_and(|value| !value.is_empty()) {
            output.push(format!(
                "  Use download_chat_attachment(message_id='{message_name}', attachment_index={index}) to download"
            ));
        }
    }
    if message
        .get("threadReply")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && let Some(thread_name) = message
            .pointer("/thread/name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    {
        output.push(format!("  [thread: {thread_name}]"));
    }
    let reactions = array(message, "emojiReactionSummaries");
    if !reactions.is_empty() {
        let parts: Vec<String> = reactions
            .iter()
            .map(|reaction| {
                let emoji = reaction.get("emoji").unwrap_or(&Value::Null);
                let symbol = api_string(emoji, "unicode")
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        format!(
                            ":{}:",
                            emoji
                                .pointer("/customEmoji/uid")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                        )
                    });
                let count = reaction
                    .get("reactionCount")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                format!("{symbol}x{count}")
            })
            .collect();
        output.push(format!("  [reactions: {}]", parts.join(", ")));
    }
    output.push(format!("  (Message ID: {message_name})\n"));
}

fn rich_links(message: &Value) -> Vec<&str> {
    let text = api_string(message, "text").unwrap_or("");
    array(message, "annotations")
        .iter()
        .filter(|annotation| api_string(annotation, "type") == Some("RICH_LINK"))
        .filter_map(|annotation| {
            annotation
                .pointer("/richLinkMetadata/uri")
                .and_then(Value::as_str)
                .filter(|uri| !uri.is_empty() && !text.contains(uri))
        })
        .collect()
}

#[derive(Default)]
struct SenderCache {
    values: HashMap<String, String>,
    order: VecDeque<String>,
}

fn sender_cache_get(key: &str) -> Option<String> {
    SENDER_CACHE
        .get_or_init(|| Mutex::new(SenderCache::default()))
        .lock()
        .ok()
        .and_then(|cache| cache.values.get(key).cloned())
}

fn sender_cache_insert(key: &str, value: &str) {
    let Ok(mut cache) = SENDER_CACHE
        .get_or_init(|| Mutex::new(SenderCache::default()))
        .lock()
    else {
        return;
    };
    if cache.values.contains_key(key) {
        cache.values.insert(key.to_owned(), value.to_owned());
        return;
    }
    if cache.values.len() >= SENDER_CACHE_CAPACITY {
        for _ in 0..(SENDER_CACHE_CAPACITY / 2) {
            if let Some(oldest) = cache.order.pop_front() {
                cache.values.remove(&oldest);
            }
        }
    }
    cache.order.push_back(key.to_owned());
    cache.values.insert(key.to_owned(), value.to_owned());
}

struct SearchHit {
    message: Value,
    space_name: String,
}

fn python_slice_end(length: usize, max_spaces: i64) -> usize {
    if max_spaces >= 0 {
        usize::try_from(max_spaces)
            .unwrap_or(usize::MAX)
            .min(length)
    } else {
        let excluded = usize::try_from(max_spaces.unsigned_abs()).unwrap_or(usize::MAX);
        length.saturating_sub(excluded)
    }
}

fn is_transient_network_error(error: &str) -> bool {
    error.contains("Google API request failed")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated: String = value.chars().take(limit).collect();
    truncated.push_str("...");
    truncated
}

async fn read_error_preview(response: &mut reqwest::Response) -> String {
    let mut preview = Vec::new();
    while preview.len() < 500 {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = 500 - preview.len();
                preview.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            _ => break,
        }
    }
    String::from_utf8_lossy(&preview).into_owned()
}

fn declared_content_length(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

async fn stream_stateless(
    response: &mut reqwest::Response,
    filename: &str,
) -> Result<(u64, String), String> {
    let mut total = 0u64;
    let mut preview_bytes = Vec::with_capacity(STATELESS_PREVIEW_INPUT_BYTES);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed to download attachment '{filename}': {error}"))?
    {
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| attachment_too_large(filename))?;
        if total > MAX_CHAT_ATTACHMENT_BYTES {
            return Err(attachment_too_large(filename));
        }
        let remaining = STATELESS_PREVIEW_INPUT_BYTES.saturating_sub(preview_bytes.len());
        preview_bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    Ok((total, urlsafe_base64(&preview_bytes)))
}

async fn stream_to_file(
    response: &mut reqwest::Response,
    filename: &str,
    mut file: File,
) -> Result<(u64, File), String> {
    let mut total = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed to download attachment '{filename}': {error}"))?
    {
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| attachment_too_large(filename))?;
        if total > MAX_CHAT_ATTACHMENT_BYTES {
            return Err(attachment_too_large(filename));
        }
        file.write_all(&chunk)
            .map_err(|error| format!("Failed to save attachment '{filename}': {error}"))?;
    }
    Ok((total, file))
}

fn attachment_too_large(filename: &str) -> String {
    format!(
        "Attachment '{filename}' exceeds the {MAX_CHAT_ATTACHMENT_BYTES} byte limit; download aborted."
    )
}

struct PreparedAttachment {
    file_id: String,
    path: PathBuf,
    file: File,
}

fn prepare_attachment_file(
    filename: &str,
    content_type: &str,
    owner: &str,
) -> Result<PreparedAttachment, String> {
    let root = attachment_storage_root()?;
    create_private_directory(&root)?;
    let root = root.canonicalize().map_err(|error| {
        format!(
            "Failed to resolve attachment storage directory {}: {error}",
            root.display()
        )
    })?;
    sweep_expired_attachments(&root);
    let owner_directory = root.join(path_segment(owner));
    create_private_directory(&owner_directory)?;
    let owner_directory = owner_directory.canonicalize().map_err(|error| {
        format!(
            "Failed to resolve attachment owner directory {}: {error}",
            owner_directory.display()
        )
    })?;
    if !owner_directory.starts_with(&root) {
        return Err(String::from(
            "Attachment owner directory resolves outside the configured storage directory",
        ));
    }

    for _ in 0..16 {
        let file_id = unique_file_id();
        let save_name = build_save_name(&file_id, filename, content_type);
        let path = owner_directory.join(save_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                set_file_permissions(&path)?;
                return Ok(PreparedAttachment {
                    file_id,
                    path,
                    file,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create attachment file {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Err(String::from(
        "Failed to allocate a unique attachment filename",
    ))
}

fn attachment_storage_root() -> Result<PathBuf, String> {
    if let Some(configured) = env::var_os("WORKSPACE_ATTACHMENT_DIR") {
        return Ok(expand_home(PathBuf::from(configured)));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| String::from("Could not determine the user home directory"))?;
    Ok(home.join(".workspace-mcp/attachments"))
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path;
    };
    if value == "~" {
        return env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or(path);
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    path
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("Failed to create directory {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "Failed to set directory permissions on {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn sweep_expired_attachments(root: &Path) {
    let Ok(owner_directories) = fs::read_dir(root) else {
        return;
    };
    for owner_directory in owner_directories.flatten() {
        let path = owner_directory.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_path = entry.path();
            let expired = entry
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age.as_secs() >= ATTACHMENT_EXPIRATION_SECONDS);
            if expired {
                let _ = fs::remove_file(file_path);
            }
        }
    }
}

fn set_file_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!(
                "Failed to set attachment permissions on {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn normalize_owner(owner: &str) -> Result<String, String> {
    let owner = owner.trim().to_lowercase();
    if owner.is_empty() {
        return Err(String::from(
            "An attachment owner is required; attachments cannot be stored without the principal they belong to.",
        ));
    }
    Ok(owner)
}

fn unique_file_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp:032x}{:08x}{sequence:016x}", std::process::id())
}

fn build_save_name(file_id: &str, filename: &str, _content_type: &str) -> String {
    let safe = sanitize_filename(filename);
    let path = Path::new(&safe);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment");
    match path.extension().and_then(|value| value.to_str()) {
        Some(extension) if !extension.is_empty() => format!("{stem}_{file_id}.{extension}"),
        _ => format!("{stem}_{file_id}"),
    }
}

fn sanitize_filename(filename: &str) -> String {
    let mut sanitized = String::new();
    for character in filename.chars() {
        if character.is_whitespace() && !character.is_ascii_whitespace() {
            sanitized.push(' ');
        } else if character.is_control() || "<>:\"/\\|?*".contains(character) {
            sanitized.push('_');
        } else {
            sanitized.push(character);
        }
    }
    while sanitized.ends_with([' ', '.']) {
        sanitized.pop();
    }
    if sanitized.is_empty() {
        return String::from("attachment");
    }
    let stem = sanitized.split('.').next().unwrap_or("");
    if matches!(
        stem.to_ascii_uppercase().as_str(),
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
        sanitized.insert(0, '_');
    }
    sanitized
}

fn is_stateless_mode() -> bool {
    env::var("WORKSPACE_MCP_STATELESS_MODE").is_ok_and(|value| value.eq_ignore_ascii_case("true"))
}

fn transport_mode() -> String {
    env::var("WORKSPACE_MCP_TRANSPORT").unwrap_or_else(|_| String::from("stdio"))
}

fn attachment_url(file_id: &str) -> Result<String, String> {
    let base = if let Ok(external) = env::var("WORKSPACE_EXTERNAL_URL")
        && !external.trim().is_empty()
    {
        external.trim_end_matches('/').to_owned()
    } else {
        let base_uri =
            env::var("WORKSPACE_MCP_BASE_URI").unwrap_or_else(|_| String::from("http://localhost"));
        let port = if env::var("WORKSPACE_MCP_RESOLVED_PORT").as_deref() == Ok("1") {
            env::var("WORKSPACE_MCP_PORT")
                .or_else(|_| env::var("PORT"))
                .unwrap_or_else(|_| String::from("8000"))
        } else {
            env::var("PORT")
                .or_else(|_| env::var("WORKSPACE_MCP_PORT"))
                .unwrap_or_else(|_| String::from("8000"))
        };
        format!("{}:{port}", base_uri.trim_end_matches('/'))
    };
    let parsed = reqwest::Url::parse(&base).map_err(|_| {
        format!(
            "Cannot build an absolute attachment URL: configure WORKSPACE_EXTERNAL_URL (or WORKSPACE_MCP_BASE_URI and the port) with a scheme and host. Got {base:?}."
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!(
            "Cannot build an absolute attachment URL from {base:?}."
        ));
    }
    Ok(format!("{base}/attachments/{file_id}"))
}

fn urlsafe_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn invalid_attachment_index(index: i64, count: usize) -> String {
    format!(
        "Invalid attachment_index {index}. Message has {count} attachment(s) (0-{}).",
        count.saturating_sub(1)
    )
}

fn validated_space(resource: &str) -> Result<String, String> {
    validate_and_encode_resource(resource, &["spaces", "*"]).map_err(|_| {
        format!("space_id must be a Chat space resource name (spaces/SPACE_ID), got '{resource}'")
    })
}

fn validated_message(resource: &str) -> Result<String, String> {
    validate_and_encode_resource(resource, &["spaces", "*", "messages", "*"]).map_err(|_| {
        format!(
            "message_id must be a Chat message resource name (spaces/SPACE_ID/messages/MESSAGE_ID), got '{resource}'"
        )
    })
}

fn validated_thread(resource: &str) -> Result<String, String> {
    validate_and_encode_resource(resource, &["spaces", "*", "threads", "*"]).map_err(|_| {
        format!(
            "thread_name must be a Chat thread resource name (spaces/SPACE_ID/threads/THREAD_ID), got '{resource}'"
        )
    })
}

fn validated_attachment(resource: &str) -> Result<String, String> {
    validate_and_encode_resource(resource, &["spaces", "*", "attachments", "*"])
        .or_else(|_| {
            validate_and_encode_resource(
                resource,
                &["spaces", "*", "messages", "*", "attachments", "*"],
            )
        })
        .map_err(|_| {
            format!(
                "Invalid Chat attachment resource name '{resource}'; expected spaces/SPACE_ID/attachments/ATTACHMENT_ID"
            )
        })
}

fn validate_and_encode_resource(resource: &str, pattern: &[&str]) -> Result<String, ()> {
    let segments: Vec<&str> = resource.split('/').collect();
    if segments.len() != pattern.len() {
        return Err(());
    }
    for (segment, expected) in segments.iter().zip(pattern) {
        if segment.is_empty()
            || *segment == "."
            || *segment == ".."
            || segment.chars().any(char::is_control)
            || (*expected != "*" && segment != expected)
        {
            return Err(());
        }
    }
    Ok(segments
        .into_iter()
        .map(path_segment)
        .collect::<Vec<_>>()
        .join("/"))
}

fn path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn array<'a>(value: &'a Value, name: &str) -> &'a [Value] {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn api_string<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

fn required_string(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    nullable_string(arguments, name)?.ok_or_else(|| format!("{name} is required"))
}

fn nullable_string(arguments: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    let Some(value) = argument(arguments, name) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| format!("{name} must be a string"))
}

fn string_with_default(
    arguments: &Map<String, Value>,
    name: &str,
    default: &str,
) -> Result<String, String> {
    match argument(arguments, name) {
        None => Ok(default.to_owned()),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{name} must be a string")),
    }
}

fn integer(arguments: &Map<String, Value>, name: &str, default: i64) -> Result<i64, String> {
    match argument(arguments, name) {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| format!("{name} must be an integer")),
    }
}

fn argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    arguments
        .get(name)
        .or_else(|| arguments.get(&snake_to_camel(name)))
}

fn validate_arguments(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    for (key, value) in arguments {
        let Some(canonical) = allowed
            .iter()
            .find(|name| key == **name || key == &snake_to_camel(name))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        let camel = snake_to_camel(canonical);
        if camel != *canonical && key == &camel && arguments.contains_key(*canonical) {
            return Err(format!("Unexpected argument: {key}"));
        }
        if value.is_null() {
            continue;
        }
        match *canonical {
            "page_size" | "max_spaces" | "attachment_index" if !value.is_i64() => {
                return Err(format!("{canonical} must be an integer"));
            }
            _ if !value.is_string() => {
                return Err(format!("{canonical} must be a string"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn snake_to_camel(value: &str) -> String {
    let mut result = String::new();
    let mut uppercase = false;
    for character in value.chars() {
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
