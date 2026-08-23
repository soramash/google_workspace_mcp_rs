//! Shared Google Drive comment operations.

use std::env;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Value, json};

use crate::auth::GoogleSession;

pub const DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
pub const DRIVE_FULL_SCOPE: &str = "https://www.googleapis.com/auth/drive";

const DRIVE_FILES_API: &str = "https://www.googleapis.com/drive/v3/files";
const COMMENT_FIELDS: &str = "nextPageToken,comments(id,content,author,createdTime,modifiedTime,resolved,quotedFileContent,replies(content,author,id,createdTime,modifiedTime))";
const COMMENT_RESULT_FIELDS: &str = "id,content,author,createdTime,modifiedTime";
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// A comment mutation after its action-specific arguments have been validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommentAction<'a> {
    Create {
        content: &'a str,
    },
    Reply {
        comment_id: &'a str,
        content: &'a str,
    },
    Resolve {
        comment_id: &'a str,
    },
}

impl<'a> CommentAction<'a> {
    /// Validate a tool action and convert it into a mutation that cannot omit required fields.
    pub fn parse(
        action: &str,
        comment_content: Option<&'a str>,
        comment_id: Option<&'a str>,
    ) -> Result<Self, String> {
        let normalized = action.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "create" => comment_content
                .filter(|value| !value.is_empty())
                .map(|content| Self::Create { content })
                .ok_or_else(|| String::from("comment_content is required for create action")),
            "reply" => match (
                comment_id.filter(|value| !value.is_empty()),
                comment_content.filter(|value| !value.is_empty()),
            ) {
                (Some(comment_id), Some(content)) => Ok(Self::Reply {
                    comment_id,
                    content,
                }),
                _ => Err(String::from(
                    "comment_id and comment_content are required for reply action",
                )),
            },
            "resolve" => comment_id
                .filter(|value| !value.is_empty())
                .map(|comment_id| Self::Resolve { comment_id })
                .ok_or_else(|| String::from("comment_id is required for resolve action")),
            _ => Err(format!(
                "Invalid action '{normalized}'. Must be 'create', 'reply', or 'resolve'."
            )),
        }
    }
}

/// Authenticate a session suitable for listing Drive comments.
pub async fn drive_readonly_session() -> Result<GoogleSession, String> {
    GoogleSession::authenticate(&[DRIVE_READONLY_SCOPE]).await
}

/// Authenticate a session suitable for creating, replying to, or resolving Drive comments.
pub async fn drive_full_session() -> Result<GoogleSession, String> {
    GoogleSession::authenticate(&[DRIVE_FULL_SCOPE]).await
}

/// List and format comments for a validated Drive file ID.
///
/// `max_comments` takes precedence over `WORKSPACE_MCP_COMMENTS_MAX`. A negative
/// limit uses the default of 100, and zero avoids making an API request.
pub async fn list_comments(
    session: &GoogleSession,
    tool_name: &str,
    app_name: &str,
    file_id: &str,
    max_comments: Option<i64>,
) -> Result<String, String> {
    let max_comments = effective_max_comments(max_comments);
    if max_comments == 0 {
        return Ok(no_comments(app_name, file_id));
    }

    let url = comments_url(file_id);
    let mut comments = Vec::new();
    let mut page_token: Option<String> = None;
    while i64::try_from(comments.len()).unwrap_or(i64::MAX) < max_comments {
        let remaining = max_comments - i64::try_from(comments.len()).unwrap_or(i64::MAX);
        let mut query = vec![
            (String::from("fields"), String::from(COMMENT_FIELDS)),
            (String::from("pageSize"), remaining.min(100).to_string()),
        ];
        if let Some(token) = &page_token {
            query.push((String::from("pageToken"), token.clone()));
        }

        let response = session
            .send_json(tool_name, session.request(Method::GET, &url).query(&query))
            .await?;
        let page = response
            .get("comments")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let take = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(page.len());
        comments.extend(page.iter().take(take).cloned());
        page_token = response
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if page_token.is_none() || i64::try_from(comments.len()).unwrap_or(i64::MAX) >= max_comments
        {
            break;
        }
    }

    if comments.is_empty() {
        Ok(no_comments(app_name, file_id))
    } else {
        Ok(format_comments(app_name, file_id, &comments))
    }
}

/// Execute a validated comment mutation and format the compatibility response.
pub async fn manage_comment(
    session: &GoogleSession,
    tool_name: &str,
    file_id: &str,
    action: CommentAction<'_>,
) -> Result<String, String> {
    match action {
        CommentAction::Create { content } => {
            create_comment(session, tool_name, file_id, content).await
        }
        CommentAction::Reply {
            comment_id,
            content,
        } => reply_to_comment(session, tool_name, file_id, comment_id, content).await,
        CommentAction::Resolve { comment_id } => {
            resolve_comment(session, tool_name, file_id, comment_id).await
        }
    }
}

pub async fn create_comment(
    session: &GoogleSession,
    tool_name: &str,
    file_id: &str,
    content: &str,
) -> Result<String, String> {
    if content.is_empty() {
        return Err(String::from(
            "comment_content is required for create action",
        ));
    }
    let result = post_comment(
        session,
        tool_name,
        &comments_url(file_id),
        json!({"content": content}),
    )
    .await?;
    Ok(format!(
        "Comment created successfully!\nComment ID: {}\nAuthor: {}\nCreated: {}\nContent: {content}",
        string_field(&result, "id", ""),
        nested_string_field(&result, &["author", "displayName"], "Unknown"),
        string_field(&result, "createdTime", "")
    ))
}

pub async fn reply_to_comment(
    session: &GoogleSession,
    tool_name: &str,
    file_id: &str,
    comment_id: &str,
    content: &str,
) -> Result<String, String> {
    if comment_id.is_empty() || content.is_empty() {
        return Err(String::from(
            "comment_id and comment_content are required for reply action",
        ));
    }
    let result = post_comment(
        session,
        tool_name,
        &replies_url(file_id, comment_id),
        json!({"content": content}),
    )
    .await?;
    Ok(format!(
        "Reply posted successfully!\nReply ID: {}\nAuthor: {}\nCreated: {}\nContent: {content}",
        string_field(&result, "id", ""),
        nested_string_field(&result, &["author", "displayName"], "Unknown"),
        string_field(&result, "createdTime", "")
    ))
}

pub async fn resolve_comment(
    session: &GoogleSession,
    tool_name: &str,
    file_id: &str,
    comment_id: &str,
) -> Result<String, String> {
    if comment_id.is_empty() {
        return Err(String::from("comment_id is required for resolve action"));
    }
    let result = post_comment(
        session,
        tool_name,
        &replies_url(file_id, comment_id),
        json!({
            "content": "This comment has been resolved.",
            "action": "resolve"
        }),
    )
    .await?;
    Ok(format!(
        "Comment {comment_id} has been resolved successfully.\nResolve reply ID: {}\nAuthor: {}\nCreated: {}",
        string_field(&result, "id", ""),
        nested_string_field(&result, &["author", "displayName"], "Unknown"),
        string_field(&result, "createdTime", "")
    ))
}

async fn post_comment(
    session: &GoogleSession,
    tool_name: &str,
    url: &str,
    body: Value,
) -> Result<Value, String> {
    session
        .send_json(
            tool_name,
            session
                .request(Method::POST, url)
                .query(&[("fields", COMMENT_RESULT_FIELDS)])
                .json(&body),
        )
        .await
}

fn effective_max_comments(value: Option<i64>) -> i64 {
    let value = value.unwrap_or_else(|| {
        env::var("WORKSPACE_MCP_COMMENTS_MAX")
            .ok()
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(100)
    });
    if value < 0 { 100 } else { value }
}

fn comments_url(file_id: &str) -> String {
    format!("{DRIVE_FILES_API}/{}/comments", path_segment(file_id))
}

fn replies_url(file_id: &str, comment_id: &str) -> String {
    format!(
        "{DRIVE_FILES_API}/{}/comments/{}/replies",
        path_segment(file_id),
        path_segment(comment_id)
    )
}

fn path_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}

fn no_comments(app_name: &str, file_id: &str) -> String {
    format!("No comments found in {app_name} {file_id}")
}

fn format_comments(app_name: &str, file_id: &str, comments: &[Value]) -> String {
    let mut output = vec![format!(
        "Found {} comments in {app_name} {file_id}:\n",
        comments.len()
    )];
    for comment in comments {
        let status = if comment
            .get("resolved")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            " [RESOLVED]"
        } else {
            ""
        };
        output.push(format!("Comment ID: {}", string_field(comment, "id", "")));
        output.push(format!(
            "Author: {}",
            nested_string_field(comment, &["author", "displayName"], "Unknown")
        ));
        output.push(format!(
            "Created: {}{status}",
            string_field(comment, "createdTime", "")
        ));
        if let Some(quoted) = comment
            .pointer("/quotedFileContent/value")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            output.push(format!("Quoted text: {quoted}"));
        }
        output.push(format!("Content: {}", string_field(comment, "content", "")));

        let replies = comment
            .get("replies")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !replies.is_empty() {
            output.push(format!("  Replies ({}):", replies.len()));
            for reply in replies {
                output.push(format!("    Reply ID: {}", string_field(reply, "id", "")));
                output.push(format!(
                    "    Author: {}",
                    nested_string_field(reply, &["author", "displayName"], "Unknown")
                ));
                output.push(format!(
                    "    Created: {}",
                    string_field(reply, "createdTime", "")
                ));
                output.push(format!(
                    "    Content: {}",
                    string_field(reply, "content", "")
                ));
            }
        }
        output.push(String::new());
    }
    output.join("\n")
}

fn string_field<'a>(value: &'a Value, field: &str, default: &'a str) -> &'a str {
    value.get(field).and_then(Value::as_str).unwrap_or(default)
}

fn nested_string_field<'a>(value: &'a Value, path: &[&str], default: &'a str) -> &'a str {
    let mut current = value;
    for part in path {
        let Some(next) = current.get(part) else {
            return default;
        };
        current = next;
    }
    current.as_str().unwrap_or(default)
}
