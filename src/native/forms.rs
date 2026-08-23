use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::auth::GoogleSession;

const FORMS_API: &str = "https://forms.googleapis.com/v1/forms";
const FORMS_BODY_SCOPE: &str = "https://www.googleapis.com/auth/forms.body";
const FORMS_BODY_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/forms.body.readonly";
const FORMS_RESPONSES_READONLY_SCOPE: &str =
    "https://www.googleapis.com/auth/forms.responses.readonly";
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub async fn dispatch(tool_name: &str, arguments: &Map<String, Value>) -> Result<String, String> {
    match tool_name {
        "create_form" => create_form(arguments).await,
        "get_form" => get_form(arguments).await,
        "set_publish_settings" => set_publish_settings(arguments).await,
        "get_form_response" => get_form_response(arguments).await,
        "list_form_responses" => list_form_responses(arguments).await,
        "batch_update_form" => batch_update_form(arguments).await,
        _ => Err(format!("native Rust tool '{tool_name}' is not implemented")),
    }
}

async fn create_form(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            "user_google_email",
            "title",
            "description",
            "document_title",
        ],
    )?;
    let title = required_string(arguments, "title")?;
    let description = optional_string(arguments, "description")?;
    let document_title = optional_string(arguments, "document_title")?;
    let session = authenticate(arguments, FORMS_BODY_SCOPE).await?;

    let mut info = Map::new();
    info.insert(String::from("title"), Value::String(title.clone()));
    if let Some(document_title) = document_title.filter(|value| !value.is_empty()) {
        info.insert(String::from("documentTitle"), Value::String(document_title));
    }
    let body = json!({ "info": info });
    let request = session.request(Method::POST, FORMS_API).json(&body);
    let created = session.send_json("create_form", request).await?;

    let form_id = field_display(&created, "formId", "None");
    let encoded_form_id = encode_path_segment(&form_id);
    if let Some(description) = description.filter(|value| !value.is_empty()) {
        let update_url = format!("{FORMS_API}/{encoded_form_id}:batchUpdate");
        let update_body = json!({
            "requests": [{
                "updateFormInfo": {
                    "info": { "description": description },
                    "updateMask": "description"
                }
            }]
        });
        let update_request = session
            .request(Method::POST, &update_url)
            .json(&update_body);
        session.send_json("create_form", update_request).await?;
    }

    let created_title = created
        .pointer("/info/title")
        .map(python_display)
        .unwrap_or(title);
    let edit_url = format!("https://docs.google.com/forms/d/{encoded_form_id}/edit");
    let responder_url = created
        .get("responderUri")
        .map(python_display)
        .unwrap_or_else(|| format!("https://docs.google.com/forms/d/{encoded_form_id}/viewform"));

    Ok(format!(
        "Successfully created form '{created_title}' for {}. Form ID: {form_id}. Edit URL: {edit_url}. Responder URL: {responder_url}",
        session.user_email
    ))
}

async fn get_form(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, &["user_google_email", "form_id"])?;
    let form_id = required_string(arguments, "form_id")?;
    let session = authenticate(arguments, FORMS_BODY_READONLY_SCOPE).await?;
    let encoded_form_id = encode_path_segment(&form_id);
    let url = format!("{FORMS_API}/{encoded_form_id}");
    let request = session.request(Method::GET, &url);
    let form = session.send_json("get_form", request).await?;

    let form_info = form
        .get("info")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let title = form_info
        .get("title")
        .map(python_display)
        .unwrap_or_else(|| String::from("No Title"));
    let description = form_info
        .get("description")
        .map(python_display)
        .unwrap_or_else(|| String::from("No Description"));
    let document_title = form_info
        .get("documentTitle")
        .map(python_display)
        .unwrap_or_else(|| title.clone());
    let edit_url = format!("https://docs.google.com/forms/d/{encoded_form_id}/edit");
    let responder_url = form
        .get("responderUri")
        .map(python_display)
        .unwrap_or_else(|| format!("https://docs.google.com/forms/d/{encoded_form_id}/viewform"));

    let items = form
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let serialized_items = items
        .iter()
        .enumerate()
        .map(|(index, item)| serialize_form_item(item, index + 1))
        .collect::<Vec<_>>();

    let items_summary_text = if serialized_items.is_empty() {
        String::from("  No items found")
    } else {
        serialized_items
            .iter()
            .map(|item| {
                let required = item.required.as_ref().is_some_and(python_truthy);
                format!(
                    "  {}. {} [{}]{}",
                    item.index,
                    python_display(&item.title),
                    item.item_type,
                    if required { " (Required)" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let items_text = if serialized_items.is_empty() {
        String::from("[]")
    } else {
        python_json_pretty(&serialized_items)?
    };

    Ok(format!(
        "Form Details for {}:\n- Title: \"{title}\"\n- Description: \"{description}\"\n- Document Title: \"{document_title}\"\n- Form ID: {form_id}\n- Edit URL: {edit_url}\n- Responder URL: {responder_url}\n- Items ({} total):\n{items_summary_text}\n- Items (structured):\n{items_text}",
        session.user_email,
        items.len()
    ))
}

async fn set_publish_settings(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &[
            "user_google_email",
            "form_id",
            "is_published",
            "is_accepting_responses",
        ],
    )?;
    let form_id = required_string(arguments, "form_id")?;
    let is_published = boolean(arguments, "is_published", true)?;
    let is_accepting_responses = boolean(arguments, "is_accepting_responses", true)?;
    let session = authenticate(arguments, FORMS_BODY_SCOPE).await?;
    let encoded_form_id = encode_path_segment(&form_id);
    let url = format!("{FORMS_API}/{encoded_form_id}:setPublishSettings");
    let body = json!({
        "publishSettings": {
            "publishState": {
                "isPublished": is_published,
                "isAcceptingResponses": is_accepting_responses
            }
        },
        "updateMask": "publishState"
    });
    let request = session.request(Method::POST, &url).json(&body);
    session.send_json("set_publish_settings", request).await?;

    Ok(format!(
        "Successfully updated publish settings for form {form_id} for {}. Published: {}, Accepting responses: {}",
        session.user_email,
        python_bool(is_published),
        python_bool(is_accepting_responses)
    ))
}

async fn get_form_response(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, &["user_google_email", "form_id", "response_id"])?;
    let form_id = required_string(arguments, "form_id")?;
    let response_id_argument = required_string(arguments, "response_id")?;
    let session = authenticate(arguments, FORMS_RESPONSES_READONLY_SCOPE).await?;
    let encoded_form_id = encode_path_segment(&form_id);
    let encoded_response_id = encode_path_segment(&response_id_argument);
    let url = format!("{FORMS_API}/{encoded_form_id}/responses/{encoded_response_id}");
    let request = session.request(Method::GET, &url);
    let response = session.send_json("get_form_response", request).await?;

    let response_id = field_display(&response, "responseId", "Unknown");
    let create_time = field_display(&response, "createTime", "Unknown");
    let last_submitted_time = field_display(&response, "lastSubmittedTime", "Unknown");
    let answer_details = response
        .get("answers")
        .and_then(Value::as_object)
        .map(|answers| {
            answers
                .iter()
                .map(|(question_id, answer_data)| {
                    let text_answers = answer_data
                        .pointer("/textAnswers/answers")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default();
                    if text_answers.is_empty() {
                        format!("  Question ID {question_id}: No answer provided")
                    } else {
                        let answer_text = text_answers
                            .iter()
                            .map(|answer| {
                                answer.get("value").map(python_display).unwrap_or_default()
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("  Question ID {question_id}: {answer_text}")
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let answers_text = if answer_details.is_empty() {
        String::from("  No answers found")
    } else {
        answer_details.join("\n")
    };

    Ok(format!(
        "Form Response Details for {}:\n- Form ID: {form_id}\n- Response ID: {response_id}\n- Created: {create_time}\n- Last Submitted: {last_submitted_time}\n- Answers:\n{answers_text}",
        session.user_email
    ))
}

async fn list_form_responses(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(
        arguments,
        &["user_google_email", "form_id", "page_size", "page_token"],
    )?;
    let form_id = required_string(arguments, "form_id")?;
    let page_size = integer(arguments, "page_size", 10)?;
    let page_token = optional_string(arguments, "page_token")?;
    let session = authenticate(arguments, FORMS_RESPONSES_READONLY_SCOPE).await?;
    let encoded_form_id = encode_path_segment(&form_id);
    let url = format!("{FORMS_API}/{encoded_form_id}/responses");
    let mut query = vec![(String::from("pageSize"), page_size.to_string())];
    if let Some(page_token) = page_token.filter(|value| !value.is_empty()) {
        query.push((String::from("pageToken"), page_token));
    }
    let request = session.request(Method::GET, &url).query(&query);
    let result = session.send_json("list_form_responses", request).await?;

    let responses = result
        .get("responses")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if responses.is_empty() {
        return Ok(format!(
            "No responses found for form {form_id} for {}.",
            session.user_email
        ));
    }

    let response_details = responses
        .iter()
        .enumerate()
        .map(|(index, response)| {
            let response_id = field_display(response, "responseId", "Unknown");
            let create_time = field_display(response, "createTime", "Unknown");
            let last_submitted_time =
                field_display(response, "lastSubmittedTime", "Unknown");
            let answers_count = response
                .get("answers")
                .and_then(Value::as_object)
                .map(Map::len)
                .unwrap_or(0);
            format!(
                "  {}. Response ID: {response_id} | Created: {create_time} | Last Submitted: {last_submitted_time} | Answers: {answers_count}",
                index + 1
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let pagination_info = result
        .get("nextPageToken")
        .filter(|value| python_truthy(value))
        .map(|token| format!("\nNext page token: {}", python_display(token)))
        .unwrap_or_else(|| String::from("\nNo more pages."));

    Ok(format!(
        "Form Responses for {}:\n- Form ID: {form_id}\n- Total responses returned: {}\n- Responses:\n{response_details}{pagination_info}",
        session.user_email,
        responses.len()
    ))
}

async fn batch_update_form(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, &["user_google_email", "form_id", "requests"])?;
    let form_id = required_string(arguments, "form_id")?;
    let requests = required_object_array(arguments, "requests")?;
    let session = authenticate(arguments, FORMS_BODY_SCOPE).await?;
    let encoded_form_id = encode_path_segment(&form_id);
    let url = format!("{FORMS_API}/{encoded_form_id}:batchUpdate");
    let body = json!({ "requests": requests });
    let request = session.request(Method::POST, &url).json(&body);
    let result = session.send_json("batch_update_form", request).await?;

    let replies = result
        .get("replies")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut output = format!(
        "Batch Update Completed:\n- Form ID: {form_id}\n- URL: https://docs.google.com/forms/d/{encoded_form_id}/edit\n- Requests Applied: {}\n- Replies Received: {}",
        requests.len(),
        replies.len()
    );
    if !replies.is_empty() {
        output.push_str("\n\nUpdate Results:");
        for (index, reply) in replies.iter().enumerate() {
            if let Some(create_item) = reply.get("createItem") {
                let item_id = create_item
                    .get("itemId")
                    .map(python_display)
                    .unwrap_or_else(|| String::from("Unknown"));
                let question_ids = create_item
                    .get("questionId")
                    .and_then(Value::as_array)
                    .map(|ids| {
                        ids.iter()
                            .map(python_display)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let question_info = if question_ids.is_empty() {
                    String::new()
                } else {
                    format!(" (Question IDs: {question_ids})")
                };
                output.push_str(&format!(
                    "\n  Request {}: Created item {item_id}{question_info}",
                    index + 1
                ));
            } else {
                output.push_str(&format!("\n  Request {}: Operation completed", index + 1));
            }
        }
    }
    Ok(output)
}

async fn authenticate(
    arguments: &Map<String, Value>,
    required_scope: &str,
) -> Result<GoogleSession, String> {
    let supplied_email = optional_principal(arguments)?;
    let session = GoogleSession::authenticate(&[required_scope]).await?;
    if let Some(supplied_email) = supplied_email
        && !supplied_email.eq_ignore_ascii_case(&session.user_email)
    {
        return Err(String::from(
            "user_google_email does not match the configured USER_GOOGLE_EMAIL",
        ));
    }
    Ok(session)
}

fn optional_principal(arguments: &Map<String, Value>) -> Result<Option<String>, String> {
    match argument(arguments, "user_google_email") {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(String::from("user_google_email must be a string")),
    }
}

fn validate_arguments(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    for key in arguments.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("Unexpected argument: {key}"));
        }
    }
    Ok(())
}

fn argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    arguments.get(name)
}

fn required_string(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    match argument(arguments, name) {
        None | Some(Value::Null) => Err(format!("{name} is required")),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

fn optional_string(arguments: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    match argument(arguments, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

fn boolean(arguments: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    match argument(arguments, name) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{name} must be a boolean")),
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

fn required_object_array<'a>(
    arguments: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a Vec<Value>, String> {
    let value = argument(arguments, name).ok_or_else(|| format!("{name} is required"))?;
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array of objects"))?;
    if values.iter().any(|value| !value.is_object()) {
        return Err(format!("{name} must contain only objects"));
    }
    Ok(values)
}

fn encode_path_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT_ENCODE_SET).to_string()
}

fn field_display(value: &Value, field: &str, default: &str) -> String {
    value
        .get(field)
        .map(python_display)
        .unwrap_or_else(|| String::from(default))
}

fn python_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

fn python_display(value: &Value) -> String {
    match value {
        Value::Null => String::from("None"),
        Value::Bool(value) => String::from(python_bool(*value)),
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn python_json_pretty<T: Serialize>(value: &T) -> Result<String, String> {
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|_| String::from("Failed to serialize form items"))?;
    let mut escaped = String::with_capacity(serialized.len());
    for character in serialized.chars() {
        let code = u32::from(character);
        if code <= 0x7f {
            escaped.push(character);
        } else if code <= 0xffff {
            escaped.push_str(&format!("\\u{code:04x}"));
        } else {
            let adjusted = code - 0x1_0000;
            let high = 0xd800 + (adjusted >> 10);
            let low = 0xdc00 + (adjusted & 0x3ff);
            escaped.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
        }
    }
    Ok(escaped)
}

#[derive(Serialize)]
struct SerializedFormItem {
    index: usize,
    #[serde(rename = "itemId")]
    item_id: Value,
    title: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<Value>,
    #[serde(rename = "type")]
    item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<Value>,
    #[serde(rename = "questionId", skip_serializing_if = "Option::is_none")]
    question_id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    grid: Option<SerializedGrid>,
}

#[derive(Serialize)]
struct SerializedGrid {
    rows: Vec<SerializedGridRow>,
    columns: Vec<Value>,
}

#[derive(Serialize)]
struct SerializedGridRow {
    title: Value,
    #[serde(rename = "questionId", skip_serializing_if = "Option::is_none")]
    question_id: Option<Value>,
    required: Value,
}

fn serialize_form_item(item: &Value, index: usize) -> SerializedFormItem {
    let item = item.as_object();
    let item_id = item
        .and_then(|item| item.get("itemId"))
        .cloned()
        .unwrap_or(Value::Null);
    let title = item
        .and_then(|item| item.get("title"))
        .cloned()
        .unwrap_or_else(|| Value::String(format!("Question {index}")));
    let description = item
        .and_then(|item| item.get("description"))
        .filter(|value| python_truthy(value))
        .cloned();

    if item.is_some_and(|item| item.contains_key("questionItem")) {
        let question = item
            .and_then(|item| item.get("questionItem"))
            .and_then(|value| value.get("question"))
            .and_then(Value::as_object);
        let choice_question = question
            .and_then(|question| question.get("choiceQuestion"))
            .filter(|value| python_truthy(value));
        let options = choice_question.map(|choice| {
            choice
                .get("options")
                .and_then(Value::as_array)
                .map(|options| extract_option_values(options))
                .unwrap_or_default()
        });
        return SerializedFormItem {
            index,
            item_id,
            title,
            description,
            item_type: question_type(question),
            required: Some(
                question
                    .and_then(|question| question.get("required"))
                    .cloned()
                    .unwrap_or(Value::Bool(false)),
            ),
            question_id: question
                .and_then(|question| question.get("questionId"))
                .filter(|value| python_truthy(value))
                .cloned(),
            options,
            grid: None,
        };
    }

    if item.is_some_and(|item| item.contains_key("questionGroupItem")) {
        let group = item
            .and_then(|item| item.get("questionGroupItem"))
            .and_then(Value::as_object);
        let columns = group
            .and_then(|group| group.get("grid"))
            .and_then(|grid| grid.get("columns"))
            .and_then(|columns| columns.get("options"))
            .and_then(Value::as_array)
            .map(|options| extract_option_values(options))
            .unwrap_or_default();
        let rows = group
            .and_then(|group| group.get("questions"))
            .and_then(Value::as_array)
            .map(|questions| {
                questions
                    .iter()
                    .map(|question| SerializedGridRow {
                        title: question
                            .get("rowQuestion")
                            .and_then(|row| row.get("title"))
                            .cloned()
                            .unwrap_or_else(|| Value::String(String::new())),
                        question_id: question
                            .get("questionId")
                            .filter(|value| python_truthy(value))
                            .cloned(),
                        required: question
                            .get("required")
                            .cloned()
                            .unwrap_or(Value::Bool(false)),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return SerializedFormItem {
            index,
            item_id,
            title,
            description,
            item_type: String::from("GRID"),
            required: None,
            question_id: None,
            options: None,
            grid: Some(SerializedGrid { rows, columns }),
        };
    }

    let item_type = if item.is_some_and(|item| item.contains_key("pageBreakItem")) {
        "PAGE_BREAK"
    } else if item.is_some_and(|item| item.contains_key("textItem")) {
        "TEXT_ITEM"
    } else if item.is_some_and(|item| item.contains_key("imageItem")) {
        "IMAGE"
    } else if item.is_some_and(|item| item.contains_key("videoItem")) {
        "VIDEO"
    } else {
        "UNKNOWN"
    };
    SerializedFormItem {
        index,
        item_id,
        title,
        description,
        item_type: String::from(item_type),
        required: None,
        question_id: None,
        options: None,
        grid: None,
    }
}

fn extract_option_values(options: &[Value]) -> Vec<Value> {
    options
        .iter()
        .filter(|option| option.get("value").is_some_and(python_truthy))
        .cloned()
        .collect()
}

fn question_type(question: Option<&Map<String, Value>>) -> String {
    if let Some(choice) = question
        .and_then(|question| question.get("choiceQuestion"))
        .filter(|value| python_truthy(value))
    {
        return choice
            .get("type")
            .map(python_display)
            .unwrap_or_else(|| String::from("CHOICE"));
    }
    if let Some(text) = question
        .and_then(|question| question.get("textQuestion"))
        .filter(|value| python_truthy(value))
    {
        return if text.get("paragraph").is_some_and(python_truthy) {
            String::from("PARAGRAPH")
        } else {
            String::from("TEXT")
        };
    }
    if question.is_some_and(|question| question.contains_key("rowQuestion")) {
        String::from("GRID_ROW")
    } else if question.is_some_and(|question| question.contains_key("scaleQuestion")) {
        String::from("SCALE")
    } else if question.is_some_and(|question| question.contains_key("dateQuestion")) {
        String::from("DATE")
    } else if question.is_some_and(|question| question.contains_key("timeQuestion")) {
        String::from("TIME")
    } else if question.is_some_and(|question| question.contains_key("fileUploadQuestion")) {
        String::from("FILE_UPLOAD")
    } else if question.is_some_and(|question| question.contains_key("ratingQuestion")) {
        String::from("RATING")
    } else {
        String::from("QUESTION")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_path_segment("form/id ?#"), "form%2Fid%20%3F%23");
    }

    #[test]
    fn batch_requests_remain_opaque() {
        let arguments = json!({
            "requests": [{
                "createItem": {
                    "item": {"title": "Question"},
                    "unknownFutureField": {"nested_value": true}
                }
            }]
        });
        let arguments = arguments.as_object().unwrap();
        let requests = required_object_array(arguments, "requests").unwrap();
        assert_eq!(
            requests[0]["createItem"]["unknownFutureField"]["nested_value"],
            true
        );
    }

    #[test]
    fn serializer_preserves_forms_edit_metadata() {
        let item = json!({
            "itemId": "item_123",
            "title": "Favorite color?",
            "questionItem": {
                "question": {
                    "questionId": "q_123",
                    "required": true,
                    "choiceQuestion": {
                        "type": "RADIO",
                        "options": [
                            {"value": "Red", "goToAction": "NEXT_SECTION"},
                            {"value": ""}
                        ]
                    }
                }
            }
        });
        let serialized = serialize_form_item(&item, 1);
        assert_eq!(serialized.item_type, "RADIO");
        assert_eq!(serialized.question_id, Some(json!("q_123")));
        assert_eq!(serialized.options.unwrap().len(), 1);
    }

    #[test]
    fn rejects_unknown_and_duplicate_arguments() {
        let unknown = json!({"form_id": "one", "extra": true});
        assert_eq!(
            validate_arguments(unknown.as_object().unwrap(), &["form_id"]),
            Err(String::from("Unexpected argument: extra"))
        );
        let duplicate = json!({"form_id": "one", "formId": "two"});
        assert_eq!(
            validate_arguments(duplicate.as_object().unwrap(), &["form_id"]),
            Err(String::from("Unexpected argument: formId"))
        );
    }
}
