use std::collections::{HashMap, HashSet};
use std::env;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::auth::GoogleSession;

const SLIDES_BASE_URL: &str = "https://slides.googleapis.com/v1/presentations";
const DRIVE_BASE_URL: &str = "https://www.googleapis.com/drive/v3/files";
const PRESENTATIONS_SCOPE: &str = "https://www.googleapis.com/auth/presentations";
const PRESENTATIONS_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/presentations.readonly";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive";
const DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
const PRESENTATION_PAGE_ID_FIELDS: &str = "slides(objectId,slideProperties(notesPage(objectId,notesProperties(speakerNotesObjectId)))),masters(objectId),layouts(objectId),notesMaster(objectId)";
const COMMENT_FIELDS: &str = "nextPageToken,comments(id,content,author,createdTime,modifiedTime,resolved,quotedFileContent,replies(content,author,id,createdTime,modifiedTime))";
const COMMENT_RESULT_FIELDS: &str = "id,content,author,createdTime,modifiedTime";

const BATCH_REQUEST_TYPES: &[&str] = &[
    "createSlide",
    "createShape",
    "createTable",
    "insertText",
    "insertTableRows",
    "insertTableColumns",
    "deleteTableRow",
    "deleteTableColumn",
    "replaceAllText",
    "deleteObject",
    "updatePageElementTransform",
    "updateSlidesPosition",
    "deleteText",
    "createImage",
    "createVideo",
    "createSheetsChart",
    "createLine",
    "refreshSheetsChart",
    "updateShapeProperties",
    "updateImageProperties",
    "updateVideoProperties",
    "updatePageProperties",
    "updateTableCellProperties",
    "updateLineProperties",
    "createParagraphBullets",
    "replaceAllShapesWithImage",
    "duplicateObject",
    "updateTextStyle",
    "replaceAllShapesWithSheetsChart",
    "deleteParagraphBullets",
    "updateParagraphStyle",
    "updateTableBorderProperties",
    "updateTableColumnProperties",
    "updateTableRowProperties",
    "mergeTableCells",
    "unmergeTableCells",
    "groupObjects",
    "ungroupObjects",
    "updatePageElementAltText",
    "replaceImage",
    "updateSlideProperties",
    "updatePageElementsZOrder",
    "updateLineCategory",
    "rerouteLine",
];

#[derive(Clone, Copy)]
enum ArgumentKind {
    String,
    Boolean,
    Array,
    NullableString,
    NullableInteger,
}

pub async fn dispatch(
    tool_name: &str,
    arguments: &serde_json::Map<String, Value>,
) -> Result<String, String> {
    match tool_name {
        "create_presentation" => create_presentation(arguments).await,
        "get_presentation" => get_presentation(arguments).await,
        "batch_update_presentation" => batch_update_presentation(arguments).await,
        "get_page" => get_page(arguments).await,
        "get_page_thumbnail" => get_page_thumbnail(arguments).await,
        "list_presentation_comments" => list_presentation_comments(arguments).await,
        "manage_presentation_comment" => manage_presentation_comment(arguments).await,
        _ => Err(format!(
            "native Rust Slides tool '{tool_name}' is not implemented"
        )),
    }
}

async fn create_presentation(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("title", ArgumentKind::String),
        ],
    )?;
    let title = optional_string(arguments, "title")
        .unwrap_or_else(|| String::from("Untitled Presentation"));
    let session = authenticated_session(arguments, PRESENTATIONS_SCOPE).await?;
    let result = session
        .send_json(
            "create_presentation",
            session
                .request(Method::POST, SLIDES_BASE_URL)
                .json(&json!({"title": title})),
        )
        .await?;
    let presentation_id = result
        .get("presentationId")
        .and_then(Value::as_str)
        .unwrap_or("None");
    let slide_count = result
        .get("slides")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    Ok(format!(
        "Presentation Created Successfully for {}:\n- Title: {title}\n- Presentation ID: {presentation_id}\n- URL: https://docs.google.com/presentation/d/{presentation_id}/edit\n- Slides: {slide_count} slide(s) created",
        session.user_email
    ))
}

async fn get_presentation(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("presentation_id", ArgumentKind::String),
            ("include_speaker_notes", ArgumentKind::Boolean),
        ],
    )?;
    let presentation_id = required_string(arguments, "presentation_id")?;
    let include_speaker_notes = optional_boolean(arguments, "include_speaker_notes", false);
    let session = authenticated_session(arguments, PRESENTATIONS_READONLY_SCOPE).await?;
    let url = format!("{SLIDES_BASE_URL}/{}", path_segment(&presentation_id));
    let result = session
        .send_json("get_presentation", session.request(Method::GET, &url))
        .await?;

    let title = result
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled");
    let slides = result
        .get("slides")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let width = result.pointer("/pageSize/width/magnitude");
    let height = result.pointer("/pageSize/height/magnitude");
    let unit = result
        .pointer("/pageSize/width/unit")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut slides_info = Vec::new();
    for (index, slide) in slides.iter().enumerate() {
        let slide_id = slide
            .get("objectId")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        let elements = slide
            .get("pageElements")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut texts = Vec::new();
        collect_text_bearing_elements(elements, &mut texts);
        let rows = texts
            .join("\n")
            .lines()
            .filter(|row| !row.trim().is_empty())
            .map(|row| format!("    > {row}"))
            .collect::<Vec<_>>();
        let slide_text = if rows.is_empty() {
            String::from("empty")
        } else {
            format!("\n{}", rows.join("\n"))
        };
        slides_info.push(format!(
            "  Slide {}: ID {slide_id}, {} element(s), text: {slide_text}",
            index + 1,
            elements.len()
        ));
        if include_speaker_notes {
            slides_info.extend(describe_speaker_notes(slide, "    "));
        }
    }
    let breakdown = if slides_info.is_empty() {
        String::from("  No slides found")
    } else {
        slides_info.join("\n")
    };

    Ok(format!(
        "Presentation Details for {}:\n- Title: {title}\n- Presentation ID: {presentation_id}\n- URL: https://docs.google.com/presentation/d/{presentation_id}/edit\n- Total Slides: {}\n- Page Size: {} x {} {unit}\n\nSlides Breakdown:\n{breakdown}",
        session.user_email,
        slides.len(),
        display_scalar(width, "Unknown"),
        display_scalar(height, "Unknown")
    ))
}

async fn batch_update_presentation(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("presentation_id", ArgumentKind::String),
            ("requests", ArgumentKind::Array),
        ],
    )?;
    let presentation_id = required_string(arguments, "presentation_id")?;
    let requests = required_array(arguments, "requests")?;
    validate_batch_update_requests(requests)?;
    let session = authenticated_session(arguments, PRESENTATIONS_SCOPE).await?;
    validate_insert_text_targets(&session, &presentation_id, requests).await?;

    let url = format!(
        "{SLIDES_BASE_URL}/{}:batchUpdate",
        path_segment(&presentation_id)
    );
    let result = session
        .send_json(
            "batch_update_presentation",
            session
                .request(Method::POST, &url)
                .json(&json!({"requests": requests})),
        )
        .await?;
    let replies = result
        .get("replies")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut output = format!(
        "Batch Update Completed for {}:\n- Presentation ID: {presentation_id}\n- URL: https://docs.google.com/presentation/d/{presentation_id}/edit\n- Requests Applied: {}\n- Replies Received: {}",
        session.user_email,
        requests.len(),
        replies.len()
    );
    if !replies.is_empty() {
        output.push_str("\n\nUpdate Results:");
        for (index, reply) in replies.iter().enumerate() {
            if let Some(created) = reply.get("createSlide") {
                let object_id = created
                    .get("objectId")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown");
                output.push_str(&format!(
                    "\n  Request {}: Created slide with ID {object_id}",
                    index + 1
                ));
            } else if let Some(created) = reply.get("createShape") {
                let object_id = created
                    .get("objectId")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown");
                output.push_str(&format!(
                    "\n  Request {}: Created shape with ID {object_id}",
                    index + 1
                ));
            } else {
                output.push_str(&format!("\n  Request {}: Operation completed", index + 1));
            }
        }
    }
    Ok(output)
}

async fn get_page(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("presentation_id", ArgumentKind::String),
            ("page_object_id", ArgumentKind::String),
        ],
    )?;
    let presentation_id = required_string(arguments, "presentation_id")?;
    let page_object_id = required_string(arguments, "page_object_id")?;
    let session = authenticated_session(arguments, PRESENTATIONS_READONLY_SCOPE).await?;
    let url = format!(
        "{SLIDES_BASE_URL}/{}/pages/{}",
        path_segment(&presentation_id),
        path_segment(&page_object_id)
    );
    let result = session
        .send_json("get_page", session.request(Method::GET, &url))
        .await?;
    let page_type = result
        .get("pageType")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let elements = result
        .get("pageElements")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut element_lines = Vec::new();
    describe_elements(elements, "  ", &mut element_lines);
    let element_details = if element_lines.is_empty() {
        String::from("  No elements found")
    } else {
        element_lines.join("\n")
    };

    Ok(format!(
        "Page Details for {}:\n- Presentation ID: {presentation_id}\n- Page ID: {page_object_id}\n- Page Type: {page_type}\n- Total Elements: {}\n\nPage Elements:\n{element_details}",
        session.user_email,
        elements.len()
    ))
}

async fn get_page_thumbnail(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("presentation_id", ArgumentKind::String),
            ("page_object_id", ArgumentKind::String),
            ("thumbnail_size", ArgumentKind::String),
        ],
    )?;
    let presentation_id = required_string(arguments, "presentation_id")?;
    let page_object_id = required_string(arguments, "page_object_id")?;
    let thumbnail_size =
        optional_string(arguments, "thumbnail_size").unwrap_or_else(|| String::from("MEDIUM"));
    let session = authenticated_session(arguments, PRESENTATIONS_READONLY_SCOPE).await?;
    let url = format!(
        "{SLIDES_BASE_URL}/{}/pages/{}/thumbnail",
        path_segment(&presentation_id),
        path_segment(&page_object_id)
    );
    let result = session
        .send_json(
            "get_page_thumbnail",
            session.request(Method::GET, &url).query(&[
                ("thumbnailProperties.thumbnailSize", thumbnail_size.as_str()),
                ("thumbnailProperties.mimeType", "PNG"),
            ]),
        )
        .await?;
    let thumbnail_url = result
        .get("contentUrl")
        .and_then(Value::as_str)
        .unwrap_or("");

    Ok(format!(
        "Thumbnail Generated for {}:\n- Presentation ID: {presentation_id}\n- Page ID: {page_object_id}\n- Thumbnail Size: {thumbnail_size}\n- Thumbnail URL: {thumbnail_url}\n\nYou can view or download the thumbnail using the provided URL.",
        session.user_email
    ))
}

async fn list_presentation_comments(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("presentation_id", ArgumentKind::String),
            ("max_comments", ArgumentKind::NullableInteger),
        ],
    )?;
    let presentation_id = required_string(arguments, "presentation_id")?;
    let mut max_comments = nullable_integer(arguments, "max_comments")?.unwrap_or_else(|| {
        env::var("WORKSPACE_MCP_COMMENTS_MAX")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(100)
    });
    if max_comments < 0 {
        max_comments = 100;
    }
    if max_comments == 0 {
        return Ok(format!(
            "No comments found in presentation {presentation_id}"
        ));
    }

    let session = authenticated_session(arguments, DRIVE_READONLY_SCOPE).await?;
    let url = format!(
        "{DRIVE_BASE_URL}/{}/comments",
        path_segment(&presentation_id)
    );
    let mut comments = Vec::new();
    let mut page_token: Option<String> = None;
    while i64::try_from(comments.len()).unwrap_or(i64::MAX) < max_comments {
        let remaining = max_comments - i64::try_from(comments.len()).unwrap_or(i64::MAX);
        let page_size = remaining.min(100);
        let mut query = vec![
            (String::from("fields"), String::from(COMMENT_FIELDS)),
            (String::from("pageSize"), page_size.to_string()),
        ];
        if let Some(token) = &page_token {
            query.push((String::from("pageToken"), token.clone()));
        }
        let result = session
            .send_json(
                "list_presentation_comments",
                session.request(Method::GET, &url).query(&query),
            )
            .await?;
        let page_comments = result
            .get("comments")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let take = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(page_comments.len());
        comments.extend(page_comments.iter().take(take).cloned());
        page_token = result
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if page_token.is_none() || i64::try_from(comments.len()).unwrap_or(i64::MAX) >= max_comments
        {
            break;
        }
    }
    if comments.is_empty() {
        return Ok(format!(
            "No comments found in presentation {presentation_id}"
        ));
    }
    Ok(format_comments(&presentation_id, &comments))
}

async fn manage_presentation_comment(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            ("user_google_email", ArgumentKind::String),
            ("presentation_id", ArgumentKind::String),
            ("action", ArgumentKind::String),
            ("comment_content", ArgumentKind::NullableString),
            ("comment_id", ArgumentKind::NullableString),
        ],
    )?;
    let presentation_id = required_string(arguments, "presentation_id")?;
    let action = required_string(arguments, "action")?.trim().to_lowercase();
    let comment_content = nullable_string(arguments, "comment_content");
    let comment_id = nullable_string(arguments, "comment_id");

    match action.as_str() {
        "create" if comment_content.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from(
                "comment_content is required for create action",
            ));
        }
        "reply"
            if comment_id.as_deref().is_none_or(str::is_empty)
                || comment_content.as_deref().is_none_or(str::is_empty) =>
        {
            return Err(String::from(
                "comment_id and comment_content are required for reply action",
            ));
        }
        "resolve" if comment_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from("comment_id is required for resolve action"));
        }
        "create" | "reply" | "resolve" => {}
        _ => {
            return Err(format!(
                "Invalid action '{action}'. Must be 'create', 'reply', or 'resolve'."
            ));
        }
    }

    let session = authenticated_session(arguments, DRIVE_SCOPE).await?;
    match action.as_str() {
        "create" => {
            let content = comment_content.unwrap_or_default();
            let url = format!(
                "{DRIVE_BASE_URL}/{}/comments",
                path_segment(&presentation_id)
            );
            let result = session
                .send_json(
                    "manage_presentation_comment",
                    session
                        .request(Method::POST, &url)
                        .query(&[("fields", COMMENT_RESULT_FIELDS)])
                        .json(&json!({"content": content})),
                )
                .await?;
            Ok(format!(
                "Comment created successfully!\nComment ID: {}\nAuthor: {}\nCreated: {}\nContent: {content}",
                string_field(&result, "id", ""),
                nested_string_field(&result, &["author", "displayName"], "Unknown"),
                string_field(&result, "createdTime", "")
            ))
        }
        "reply" => {
            let content = comment_content.unwrap_or_default();
            let comment_id = comment_id.unwrap_or_default();
            let url = format!(
                "{DRIVE_BASE_URL}/{}/comments/{}/replies",
                path_segment(&presentation_id),
                path_segment(&comment_id)
            );
            let result = session
                .send_json(
                    "manage_presentation_comment",
                    session
                        .request(Method::POST, &url)
                        .query(&[("fields", COMMENT_RESULT_FIELDS)])
                        .json(&json!({"content": content})),
                )
                .await?;
            Ok(format!(
                "Reply posted successfully!\nReply ID: {}\nAuthor: {}\nCreated: {}\nContent: {content}",
                string_field(&result, "id", ""),
                nested_string_field(&result, &["author", "displayName"], "Unknown"),
                string_field(&result, "createdTime", "")
            ))
        }
        "resolve" => {
            let comment_id = comment_id.unwrap_or_default();
            let url = format!(
                "{DRIVE_BASE_URL}/{}/comments/{}/replies",
                path_segment(&presentation_id),
                path_segment(&comment_id)
            );
            let result = session
                .send_json(
                    "manage_presentation_comment",
                    session
                        .request(Method::POST, &url)
                        .query(&[("fields", COMMENT_RESULT_FIELDS)])
                        .json(&json!({
                            "content": "This comment has been resolved.",
                            "action": "resolve"
                        })),
                )
                .await?;
            Ok(format!(
                "Comment {comment_id} has been resolved successfully.\nResolve reply ID: {}\nAuthor: {}\nCreated: {}",
                string_field(&result, "id", ""),
                nested_string_field(&result, &["author", "displayName"], "Unknown"),
                string_field(&result, "createdTime", "")
            ))
        }
        _ => unreachable!("action was validated above"),
    }
}

async fn authenticated_session(
    arguments: &Map<String, Value>,
    required_scope: &str,
) -> Result<GoogleSession, String> {
    let session = GoogleSession::authenticate(&[required_scope]).await?;
    if let Some(supplied) = optional_string(arguments, "user_google_email")
        && !supplied.eq_ignore_ascii_case(&session.user_email)
    {
        return Err(String::from(
            "user_google_email does not match the configured USER_GOOGLE_EMAIL",
        ));
    }
    Ok(session)
}

fn validate_arguments(
    arguments: &Map<String, Value>,
    allowed: &[(&str, ArgumentKind)],
) -> Result<(), String> {
    for (key, value) in arguments {
        let Some((canonical, kind)) = allowed
            .iter()
            .find(|(name, _)| key == *name || key == &snake_to_camel(name))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        let camel = snake_to_camel(canonical);
        if camel != *canonical && key == &camel && arguments.contains_key(*canonical) {
            return Err(format!("Unexpected argument: {key}"));
        }
        let valid = match kind {
            ArgumentKind::String => value.is_string(),
            ArgumentKind::Boolean => value.is_boolean(),
            ArgumentKind::Array => value.is_array(),
            ArgumentKind::NullableString => value.is_null() || value.is_string(),
            ArgumentKind::NullableInteger => value.is_null() || value.as_i64().is_some(),
        };
        if !valid {
            let expected = match kind {
                ArgumentKind::String | ArgumentKind::NullableString => "a string",
                ArgumentKind::Boolean => "a boolean",
                ArgumentKind::Array => "an array",
                ArgumentKind::NullableInteger => "an integer",
            };
            return Err(format!("{canonical} must be {expected}"));
        }
    }
    Ok(())
}

fn argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    arguments
        .get(name)
        .or_else(|| arguments.get(&snake_to_camel(name)))
}

fn required_string(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    argument(arguments, name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{name} is required"))
}

fn optional_string(arguments: &Map<String, Value>, name: &str) -> Option<String> {
    argument(arguments, name)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn nullable_string(arguments: &Map<String, Value>, name: &str) -> Option<String> {
    optional_string(arguments, name)
}

fn optional_boolean(arguments: &Map<String, Value>, name: &str, default: bool) -> bool {
    argument(arguments, name)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn nullable_integer(arguments: &Map<String, Value>, name: &str) -> Result<Option<i64>, String> {
    match argument(arguments, name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer")),
    }
}

fn required_array<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a Vec<Value>, String> {
    argument(arguments, name)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{name} is required"))
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

fn path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn validate_batch_update_requests(requests: &[Value]) -> Result<(), String> {
    let guidance = "exactly one Slides request type such as createSlide, createShape, insertText, updateTextStyle, createImage, deleteObject";
    if requests.is_empty() {
        return Err(format!(
            "Invalid Slides batch update request: requests must contain at least one request object with {guidance}."
        ));
    }
    for (index, request) in requests.iter().enumerate() {
        let Some(request) = request.as_object() else {
            return Err(format!(
                "Invalid Slides batch update request: requests[{index}] must be an object containing {guidance}."
            ));
        };
        if request.len() != 1 {
            let problem = if request.is_empty() {
                String::from("is empty")
            } else {
                format!(
                    "contains multiple fields ({})",
                    request.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            return Err(format!(
                "Invalid Slides batch update request: requests[{index}] {problem}; it must contain {guidance}."
            ));
        }
        let (request_type, payload) = request.iter().next().expect("map has one entry");
        if !BATCH_REQUEST_TYPES.contains(&request_type.as_str()) {
            return Err(format!(
                "Invalid Slides batch update request: requests[{index}] has unsupported request type '{request_type}'. It must contain {guidance}."
            ));
        }
        if !payload.is_object() {
            return Err(format!(
                "Invalid Slides batch update request: requests[{index}].{request_type} must be an object for {guidance}."
            ));
        }
    }
    Ok(())
}

async fn validate_insert_text_targets(
    session: &GoogleSession,
    presentation_id: &str,
    requests: &[Value],
) -> Result<(), String> {
    let insert_targets = requests
        .iter()
        .enumerate()
        .filter_map(|(index, request)| {
            request
                .get("insertText")
                .and_then(|payload| payload.get("objectId"))
                .and_then(Value::as_str)
                .filter(|object_id| !object_id.is_empty())
                .map(|object_id| (index, object_id.to_owned()))
        })
        .collect::<Vec<_>>();
    if insert_targets.is_empty() {
        return Ok(());
    }

    let mut page_ids = requests
        .iter()
        .filter_map(|request| {
            request
                .get("createSlide")
                .and_then(|payload| payload.get("objectId"))
                .and_then(Value::as_str)
                .filter(|object_id| !object_id.is_empty())
                .map(str::to_owned)
        })
        .collect::<HashSet<_>>();
    let url = format!("{SLIDES_BASE_URL}/{}", path_segment(presentation_id));
    let presentation = session
        .send_json(
            "batch_update_presentation",
            session
                .request(Method::GET, &url)
                .query(&[("fields", PRESENTATION_PAGE_ID_FIELDS)]),
        )
        .await?;
    let mut speaker_notes_shapes = HashMap::new();
    for page_type in ["slides", "masters", "layouts"] {
        if let Some(pages) = presentation.get(page_type).and_then(Value::as_array) {
            for page in pages {
                if let Some(object_id) = page.get("objectId").and_then(Value::as_str) {
                    page_ids.insert(object_id.to_owned());
                }
            }
        }
    }
    if let Some(slides) = presentation.get("slides").and_then(Value::as_array) {
        for slide in slides {
            let Some(notes_page) = slide.pointer("/slideProperties/notesPage") else {
                continue;
            };
            let Some(notes_id) = notes_page.get("objectId").and_then(Value::as_str) else {
                continue;
            };
            page_ids.insert(notes_id.to_owned());
            if let Some(shape_id) = notes_page
                .pointer("/notesProperties/speakerNotesObjectId")
                .and_then(Value::as_str)
                .filter(|shape_id| !shape_id.is_empty())
            {
                speaker_notes_shapes.insert(notes_id.to_owned(), shape_id.to_owned());
            }
        }
    }
    if let Some(notes_master_id) = presentation
        .pointer("/notesMaster/objectId")
        .and_then(Value::as_str)
    {
        page_ids.insert(notes_master_id.to_owned());
    }

    let invalid_targets = insert_targets
        .into_iter()
        .filter(|(_, object_id)| page_ids.contains(object_id))
        .collect::<Vec<_>>();
    if invalid_targets.is_empty() {
        return Ok(());
    }
    let mut problems = Vec::new();
    let mut has_non_notes_target = false;
    for (index, object_id) in invalid_targets {
        let reference = format!("requests[{index}].insertText.objectId='{object_id}'");
        if let Some(shape_id) = speaker_notes_shapes.get(&object_id) {
            problems.push(format!(
                "{reference} targets a notes page. Speaker notes text lives in the notes shape on that page, so use objectId '{shape_id}' instead."
            ));
        } else {
            has_non_notes_target = true;
            problems.push(format!("{reference} targets a slide/page object."));
        }
    }
    let mut message = format!(
        "Invalid Slides batch update request: {}",
        problems.join(" ")
    );
    if has_non_notes_target {
        message.push_str(
            " The Slides API only allows insertText on text-capable shapes or table cells. Create a text box or shape first with createShape, set elementProperties.pageObjectId to the slide ID, then insertText into the new shape objectId. For existing content, call get_page and use a Shape or Table element ID, not the Page ID.",
        );
    }
    Err(message)
}

fn extract_shape_text(shape: Option<&Value>) -> String {
    let Some(elements) = shape
        .and_then(|shape| shape.get("text"))
        .and_then(|text| text.get("textElements"))
        .and_then(Value::as_array)
    else {
        return String::new();
    };
    let mut runs = elements
        .iter()
        .filter_map(|element| {
            let content = element
                .pointer("/textRun/content")
                .and_then(Value::as_str)
                .filter(|content| !content.is_empty())?;
            let start = element
                .get("startIndex")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            Some((start, content))
        })
        .collect::<Vec<_>>();
    runs.sort_by_key(|(start, _)| *start);
    runs.into_iter().map(|(_, content)| content).collect()
}

fn collect_text_bearing_elements(elements: &[Value], output: &mut Vec<String>) {
    for element in elements {
        if element.get("shape").is_some() {
            let text = extract_shape_text(element.get("shape"));
            if !text.is_empty() {
                output.push(text);
            }
        } else if let Some(children) = element
            .pointer("/elementGroup/children")
            .and_then(Value::as_array)
        {
            collect_text_bearing_elements(children, output);
        }
    }
}

fn describe_speaker_notes(slide: &Value, indent: &str) -> Vec<String> {
    let Some(notes_object_id) = slide
        .pointer("/slideProperties/notesPage/notesProperties/speakerNotesObjectId")
        .and_then(Value::as_str)
    else {
        return vec![format!(
            "{indent}Speaker Notes: none (slide has no notes placeholder)"
        )];
    };
    let notes_text = slide
        .pointer("/slideProperties/notesPage/pageElements")
        .and_then(Value::as_array)
        .and_then(|elements| {
            elements.iter().find(|element| {
                element.get("objectId").and_then(Value::as_str) == Some(notes_object_id)
            })
        })
        .map(|element| extract_shape_text(element.get("shape")))
        .unwrap_or_default();
    let lines = notes_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim_end())
        .collect::<Vec<_>>();
    let header = format!("{indent}Speaker Notes Shape ID: {notes_object_id}");
    if lines.is_empty() {
        vec![format!("{header}, Notes: empty")]
    } else {
        let mut output = vec![format!("{header}, Notes:")];
        output.extend(lines.into_iter().map(|line| format!("{indent}  > {line}")));
        output
    }
}

fn describe_elements(elements: &[Value], indent: &str, output: &mut Vec<String>) {
    for element in elements {
        let element_id = element
            .get("objectId")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        if let Some(shape) = element.get("shape") {
            let shape_type = shape
                .get("shapeType")
                .and_then(Value::as_str)
                .unwrap_or("Unknown");
            let full_text = extract_shape_text(Some(shape));
            let lines = full_text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| line.trim_end())
                .collect::<Vec<_>>();
            if lines.len() == 1 {
                output.push(format!(
                    "{indent}Shape: ID {element_id}, Type: {shape_type}, Text: \"{}\"",
                    lines[0]
                ));
            } else if lines.is_empty() {
                output.push(format!(
                    "{indent}Shape: ID {element_id}, Type: {shape_type}"
                ));
            } else {
                output.push(format!(
                    "{indent}Shape: ID {element_id}, Type: {shape_type}, Text:"
                ));
                output.extend(lines.into_iter().map(|line| format!("{indent}  > {line}")));
            }
        } else if let Some(table) = element.get("table") {
            output.push(format!(
                "{indent}Table: ID {element_id}, Size: {}x{}",
                display_scalar(table.get("rows"), "0"),
                display_scalar(table.get("columns"), "0")
            ));
        } else if let Some(line) = element.get("line") {
            output.push(format!(
                "{indent}Line: ID {element_id}, Type: {}",
                string_field(line, "lineType", "Unknown")
            ));
        } else if let Some(chart) = element.get("sheetsChart") {
            output.push(format!(
                "{indent}SheetsChart: ID {element_id}, SpreadsheetID {}, ChartID {}",
                display_scalar(chart.get("spreadsheetId"), "Unknown"),
                display_scalar(chart.get("chartId"), "Unknown")
            ));
        } else if let Some(image) = element.get("image") {
            let source = image
                .get("sourceUrl")
                .and_then(Value::as_str)
                .or_else(|| image.get("contentUrl").and_then(Value::as_str))
                .unwrap_or("Unknown");
            let label = if image.get("sourceUrl").and_then(Value::as_str).is_some() {
                "Source"
            } else if image.get("contentUrl").and_then(Value::as_str).is_some() {
                "ContentURL"
            } else {
                "Source"
            };
            output.push(format!("{indent}Image: ID {element_id}, {label}: {source}"));
        } else if let Some(video) = element.get("video") {
            output.push(format!(
                "{indent}Video: ID {element_id}, Source: {}, VideoID: {}",
                display_scalar(video.get("source"), "Unknown"),
                display_scalar(video.get("id"), "Unknown")
            ));
        } else if let Some(word_art) = element.get("wordArt") {
            let rendered = word_art
                .get("renderedText")
                .and_then(Value::as_str)
                .unwrap_or("");
            if rendered.is_empty() {
                output.push(format!("{indent}WordArt: ID {element_id}"));
            } else {
                output.push(format!(
                    "{indent}WordArt: ID {element_id}, Text: \"{rendered}\""
                ));
            }
        } else if let Some(group) = element.get("elementGroup") {
            let children = group
                .get("children")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            output.push(format!(
                "{indent}Group: ID {element_id}, Children: {}",
                children.len()
            ));
            describe_elements(children, &format!("{indent}  "), output);
        } else {
            output.push(format!("{indent}Element: ID {element_id}, Type: Unknown"));
        }
    }
}

fn format_comments(presentation_id: &str, comments: &[Value]) -> String {
    let mut output = vec![format!(
        "Found {} comments in presentation {presentation_id}:\n",
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
        if let Some(quoted_text) = comment
            .pointer("/quotedFileContent/value")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            output.push(format!("Quoted text: {quoted_text}"));
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
        let Some(next) = current.get(*part) else {
            return default;
        };
        current = next;
    }
    current.as_str().unwrap_or(default)
}

fn display_scalar(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => default.to_owned(),
    }
}
