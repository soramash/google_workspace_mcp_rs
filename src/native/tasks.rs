use std::collections::HashMap;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Map, Value, json};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::auth::GoogleSession;

const TASKS_API: &str = "https://tasks.googleapis.com/tasks/v1";
const TASKS_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/tasks.readonly";
const TASKS_SCOPE: &str = "https://www.googleapis.com/auth/tasks";
const LIST_TASKS_MAX_RESULTS_DEFAULT: i64 = 20;
const LIST_TASKS_MAX_RESULTS_MAX: i64 = 10_000;

const LIST_TASK_LISTS_ARGUMENTS: &[&str] = &["user_google_email", "max_results", "page_token"];
const GET_TASK_LIST_ARGUMENTS: &[&str] = &["user_google_email", "task_list_id"];
const MANAGE_TASK_LIST_ARGUMENTS: &[&str] =
    &["user_google_email", "action", "task_list_id", "title"];
const LIST_TASKS_ARGUMENTS: &[&str] = &[
    "user_google_email",
    "task_list_id",
    "max_results",
    "page_token",
    "show_completed",
    "show_deleted",
    "show_hidden",
    "show_assigned",
    "completed_max",
    "completed_min",
    "due_max",
    "due_min",
    "updated_min",
];
const GET_TASK_ARGUMENTS: &[&str] = &["user_google_email", "task_list_id", "task_id"];
const MANAGE_TASK_ARGUMENTS: &[&str] = &[
    "user_google_email",
    "action",
    "task_list_id",
    "task_id",
    "title",
    "notes",
    "status",
    "due",
    "parent",
    "previous",
    "destination_task_list",
];

pub async fn dispatch(
    tool_name: &str,
    arguments: &Map<String, Value>,
    restrict_manage_actions: bool,
) -> Result<String, String> {
    match tool_name {
        "list_task_lists" => list_task_lists(arguments).await,
        "get_task_list" => get_task_list(arguments).await,
        "manage_task_list" => manage_task_list(arguments, restrict_manage_actions).await,
        "list_tasks" => list_tasks(arguments).await,
        "get_task" => get_task(arguments).await,
        "manage_task" => manage_task(arguments, restrict_manage_actions).await,
        _ => Err(format!("Unsupported native Tasks tool: {tool_name}")),
    }
}

async fn list_task_lists(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, LIST_TASK_LISTS_ARGUMENTS)?;
    let max_results = integer(arguments, "max_results", 1000)?;
    let page_token = optional_string(arguments, "page_token")?;
    let session = authenticated_session(arguments, TASKS_READONLY_SCOPE).await?;

    let mut query = vec![(String::from("maxResults"), max_results.to_string())];
    push_nonempty(&mut query, "pageToken", page_token.as_deref());
    let result = send(
        &session,
        "list_task_lists",
        Method::GET,
        &format!("{TASKS_API}/users/@me/lists"),
        &query,
        None,
    )
    .await?;

    let task_lists = result
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if task_lists.is_empty() {
        return Ok(format!("No task lists found for {}.", session.user_email));
    }

    let mut response = format!("Task Lists for {}:\n", session.user_email);
    for task_list in task_lists {
        response.push_str(&format!(
            "- {} (ID: {})\n  Updated: {}\n",
            required_api_string(task_list, "title")?,
            required_api_string(task_list, "id")?,
            api_string(task_list, "updated").unwrap_or("N/A")
        ));
    }
    if let Some(next_page_token) = api_string(&result, "nextPageToken") {
        response.push_str(&format!("\nNext page token: {next_page_token}"));
    }
    Ok(response)
}

async fn get_task_list(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, GET_TASK_LIST_ARGUMENTS)?;
    let task_list_id = required_string(arguments, "task_list_id")?;
    let session = authenticated_session(arguments, TASKS_READONLY_SCOPE).await?;
    let result = send(
        &session,
        "get_task_list",
        Method::GET,
        &format!(
            "{TASKS_API}/users/@me/lists/{}",
            path_segment(&task_list_id)
        ),
        &[],
        None,
    )
    .await?;

    Ok(format!(
        "Task List Details for {}:\n- Title: {}\n- ID: {}\n- Updated: {}\n- Self Link: {}",
        session.user_email,
        required_api_string(&result, "title")?,
        required_api_string(&result, "id")?,
        api_string(&result, "updated").unwrap_or("N/A"),
        api_string(&result, "selfLink").unwrap_or("N/A")
    ))
}

async fn manage_task_list(
    arguments: &Map<String, Value>,
    restrict_manage_actions: bool,
) -> Result<String, String> {
    validate_arguments(arguments, MANAGE_TASK_LIST_ARGUMENTS)?;
    let action = required_string(arguments, "action")?;
    let task_list_id = optional_string(arguments, "task_list_id")?;
    let title = optional_string(arguments, "title")?;
    let valid_actions = ["create", "update", "delete", "clear_completed"];
    if !valid_actions.contains(&action.as_str()) {
        return Err(format!(
            "Invalid action '{action}'. Must be one of: create, update, delete, clear_completed"
        ));
    }
    deny_restricted_action(&action, restrict_manage_actions)?;

    match action.as_str() {
        "create" if title.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from("'title' is required for the 'create' action."));
        }
        "update" if task_list_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from(
                "'task_list_id' is required for the 'update' action.",
            ));
        }
        "update" if title.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from("'title' is required for the 'update' action."));
        }
        "delete" if task_list_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from(
                "'task_list_id' is required for the 'delete' action.",
            ));
        }
        "clear_completed" if task_list_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from(
                "'task_list_id' is required for the 'clear_completed' action.",
            ));
        }
        _ => {}
    }

    let session = authenticated_session(arguments, TASKS_SCOPE).await?;
    match action.as_str() {
        "create" => {
            let title = title.expect("validated title");
            let result = send(
                &session,
                "manage_task_list",
                Method::POST,
                &format!("{TASKS_API}/users/@me/lists"),
                &[],
                Some(&json!({"title": title})),
            )
            .await?;
            Ok(format!(
                "Task List Created for {}:\n- Title: {}\n- ID: {}\n- Created: {}\n- Self Link: {}",
                session.user_email,
                required_api_string(&result, "title")?,
                required_api_string(&result, "id")?,
                api_string(&result, "updated").unwrap_or("N/A"),
                api_string(&result, "selfLink").unwrap_or("N/A")
            ))
        }
        "update" => {
            let task_list_id = task_list_id.expect("validated task_list_id");
            let title = title.expect("validated title");
            let result = send(
                &session,
                "manage_task_list",
                Method::PUT,
                &format!(
                    "{TASKS_API}/users/@me/lists/{}",
                    path_segment(&task_list_id)
                ),
                &[],
                Some(&json!({"id": task_list_id, "title": title})),
            )
            .await?;
            Ok(format!(
                "Task List Updated for {}:\n- Title: {}\n- ID: {}\n- Updated: {}",
                session.user_email,
                required_api_string(&result, "title")?,
                required_api_string(&result, "id")?,
                api_string(&result, "updated").unwrap_or("N/A")
            ))
        }
        "delete" => {
            let task_list_id = task_list_id.expect("validated task_list_id");
            send(
                &session,
                "manage_task_list",
                Method::DELETE,
                &format!(
                    "{TASKS_API}/users/@me/lists/{}",
                    path_segment(&task_list_id)
                ),
                &[],
                None,
            )
            .await?;
            Ok(format!(
                "Task list {task_list_id} has been deleted for {}. All tasks in this list have also been deleted.",
                session.user_email
            ))
        }
        "clear_completed" => {
            let task_list_id = task_list_id.expect("validated task_list_id");
            send(
                &session,
                "manage_task_list",
                Method::POST,
                &format!("{TASKS_API}/lists/{}/clear", path_segment(&task_list_id)),
                &[],
                None,
            )
            .await?;
            Ok(format!(
                "All completed tasks have been cleared from task list {task_list_id} for {}. The tasks are now hidden and won't appear in default task list views.",
                session.user_email
            ))
        }
        _ => unreachable!(),
    }
}

async fn list_tasks(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, LIST_TASKS_ARGUMENTS)?;
    let task_list_id = required_string(arguments, "task_list_id")?;
    let max_results = integer(arguments, "max_results", LIST_TASKS_MAX_RESULTS_DEFAULT)?;
    let page_token = optional_string(arguments, "page_token")?;
    let show_completed = boolean(arguments, "show_completed", true)?;
    let show_deleted = boolean(arguments, "show_deleted", false)?;
    let show_hidden = boolean(arguments, "show_hidden", false)?;
    let show_assigned = boolean(arguments, "show_assigned", false)?;
    let completed_max = optional_string(arguments, "completed_max")?;
    let completed_min = optional_string(arguments, "completed_min")?;
    let due_max = optional_string(arguments, "due_max")?.map(|value| adjust_due_max(&value));
    let due_min = optional_string(arguments, "due_min")?;
    let updated_min = optional_string(arguments, "updated_min")?;
    let session = authenticated_session(arguments, TASKS_READONLY_SCOPE).await?;
    let url = format!("{TASKS_API}/lists/{}/tasks", path_segment(&task_list_id));

    let mut base_query = vec![
        (String::from("showCompleted"), show_completed.to_string()),
        (String::from("showDeleted"), show_deleted.to_string()),
        (String::from("showHidden"), show_hidden.to_string()),
        (String::from("showAssigned"), show_assigned.to_string()),
    ];
    push_nonempty(&mut base_query, "completedMax", completed_max.as_deref());
    push_nonempty(&mut base_query, "completedMin", completed_min.as_deref());
    push_nonempty(&mut base_query, "dueMax", due_max.as_deref());
    push_nonempty(&mut base_query, "dueMin", due_min.as_deref());
    push_nonempty(&mut base_query, "updatedMin", updated_min.as_deref());

    let mut query = base_query.clone();
    query.push((String::from("maxResults"), max_results.to_string()));
    push_nonempty(&mut query, "pageToken", page_token.as_deref());
    let mut result = send(&session, "list_tasks", Method::GET, &url, &query, None).await?;
    let mut tasks = result
        .get_mut("items")
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    let mut next_page_token = api_string(&result, "nextPageToken").map(str::to_owned);

    let mut results_remaining = if max_results != 0 {
        max_results.min(LIST_TASKS_MAX_RESULTS_MAX)
    } else {
        LIST_TASKS_MAX_RESULTS_DEFAULT
    };
    results_remaining -= i64::try_from(tasks.len()).unwrap_or(i64::MAX);
    while results_remaining > 0 {
        let Some(token) = next_page_token.clone() else {
            break;
        };
        let mut page_query = base_query.clone();
        page_query.push((String::from("maxResults"), results_remaining.to_string()));
        page_query.push((String::from("pageToken"), token));
        let mut page = send(&session, "list_tasks", Method::GET, &url, &page_query, None).await?;
        let more_tasks = page
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .map(std::mem::take)
            .unwrap_or_default();
        next_page_token = api_string(&page, "nextPageToken").map(str::to_owned);
        if more_tasks.is_empty() {
            break;
        }
        results_remaining -= i64::try_from(more_tasks.len()).unwrap_or(i64::MAX);
        tasks.extend(more_tasks);
    }

    if tasks.is_empty() {
        return Ok(format!(
            "No tasks found in task list {task_list_id} for {}.",
            session.user_email
        ));
    }

    let structured = structure_tasks(&tasks)?;
    let mut response = format!("Tasks in list {task_list_id} for {}:\n", session.user_email);
    serialize_tasks(
        &structured.roots,
        &structured.nodes,
        &structured.children,
        0,
        &mut response,
    );
    if structured.placeholder_count > 0 {
        response.push_str(&format!(
            "\n{} tasks with title Unknown parent are included as placeholders.\nThese placeholders contain subtasks whose parents were not present in the task list.\nThis can occur due to pagination. Callers can often avoid this problem if max_results is large enough to contain all tasks (subtasks and their parents) without paging.\nThis can also occur due to filtering that excludes parent tasks while including their subtasks or due to deleted or hidden parent tasks.\n",
            structured.placeholder_count
        ));
    }
    if let Some(next_page_token) = next_page_token {
        response.push_str(&format!("Next page token: {next_page_token}\n"));
    }
    Ok(response)
}

async fn get_task(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, GET_TASK_ARGUMENTS)?;
    let task_list_id = required_string(arguments, "task_list_id")?;
    let task_id = required_string(arguments, "task_id")?;
    let session = authenticated_session(arguments, TASKS_READONLY_SCOPE).await?;
    let result = send(
        &session,
        "get_task",
        Method::GET,
        &task_url(&task_list_id, &task_id),
        &[],
        None,
    )
    .await?;

    let mut response = format!(
        "Task Details for {}:\n- Title: {}\n- ID: {}\n- Status: {}\n- Updated: {}",
        session.user_email,
        api_string(&result, "title").unwrap_or("Untitled"),
        required_api_string(&result, "id")?,
        api_string(&result, "status").unwrap_or("N/A"),
        api_string(&result, "updated").unwrap_or("N/A")
    );
    append_present(&mut response, "Due Date", api_string(&result, "due"));
    append_present(&mut response, "Completed", api_string(&result, "completed"));
    append_present(&mut response, "Notes", api_string(&result, "notes"));
    append_present(
        &mut response,
        "Parent Task ID",
        api_string(&result, "parent"),
    );
    append_present(&mut response, "Position", api_string(&result, "position"));
    append_present(&mut response, "Self Link", api_string(&result, "selfLink"));
    append_present(
        &mut response,
        "Web View Link",
        api_string(&result, "webViewLink"),
    );
    Ok(response)
}

async fn manage_task(
    arguments: &Map<String, Value>,
    restrict_manage_actions: bool,
) -> Result<String, String> {
    validate_arguments(arguments, MANAGE_TASK_ARGUMENTS)?;
    let action = required_string(arguments, "action")?;
    let task_list_id = required_string(arguments, "task_list_id")?;
    let task_id = optional_string(arguments, "task_id")?;
    let title = optional_string(arguments, "title")?;
    let notes = optional_string(arguments, "notes")?;
    let status = optional_string(arguments, "status")?;
    let due = optional_string(arguments, "due")?;
    let parent = optional_string(arguments, "parent")?;
    let previous = optional_string(arguments, "previous")?;
    let destination_task_list = optional_string(arguments, "destination_task_list")?;

    if let Some(status) = status.as_deref()
        && !["needsAction", "completed"].contains(&status)
    {
        return Err(String::from(
            "invalid status: must be 'needsAction' or 'completed'",
        ));
    }
    if let Some(due) = due.as_deref() {
        validate_due(due)?;
    }
    let valid_actions = ["create", "update", "delete", "move"];
    if !valid_actions.contains(&action.as_str()) {
        return Err(format!(
            "Invalid action '{action}'. Must be one of: create, update, delete, move"
        ));
    }
    deny_restricted_action(&action, restrict_manage_actions)?;

    match action.as_str() {
        "create" if status.is_some() => {
            return Err(String::from(
                "'status' is only supported for the 'update' action.",
            ));
        }
        "create" if title.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from("'title' is required for the 'create' action."));
        }
        "update" if task_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from(
                "'task_id' is required for the 'update' action.",
            ));
        }
        "delete" if task_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from(
                "'task_id' is required for the 'delete' action.",
            ));
        }
        "move" if task_id.as_deref().is_none_or(str::is_empty) => {
            return Err(String::from("'task_id' is required for the 'move' action."));
        }
        _ => {}
    }

    let session = authenticated_session(arguments, TASKS_SCOPE).await?;
    match action.as_str() {
        "create" => {
            let title = title.expect("validated title");
            let mut body = Map::new();
            body.insert(String::from("title"), Value::String(title));
            if let Some(notes) = notes.filter(|value| !value.is_empty()) {
                body.insert(String::from("notes"), Value::String(notes));
            }
            if let Some(due) = due.filter(|value| !value.is_empty()) {
                body.insert(String::from("due"), Value::String(due));
            }
            let mut query = Vec::new();
            push_nonempty(&mut query, "parent", parent.as_deref());
            push_nonempty(&mut query, "previous", previous.as_deref());
            let result = send(
                &session,
                "manage_task",
                Method::POST,
                &format!("{TASKS_API}/lists/{}/tasks", path_segment(&task_list_id)),
                &query,
                Some(&Value::Object(body)),
            )
            .await?;
            let mut response = format!(
                "Task Created for {}:\n- Title: {}\n- ID: {}\n- Status: {}\n- Updated: {}",
                session.user_email,
                required_api_string(&result, "title")?,
                required_api_string(&result, "id")?,
                api_string(&result, "status").unwrap_or("N/A"),
                api_string(&result, "updated").unwrap_or("N/A")
            );
            append_present(&mut response, "Due Date", api_string(&result, "due"));
            append_present(&mut response, "Notes", api_string(&result, "notes"));
            append_present(
                &mut response,
                "Web View Link",
                api_string(&result, "webViewLink"),
            );
            Ok(response)
        }
        "update" => {
            let task_id = task_id.expect("validated task_id");
            let url = task_url(&task_list_id, &task_id);
            let current = send(&session, "manage_task", Method::GET, &url, &[], None).await?;
            let mut body = Map::new();
            body.insert(String::from("id"), Value::String(task_id.clone()));
            body.insert(
                String::from("title"),
                Value::String(
                    title.unwrap_or_else(|| api_string(&current, "title").unwrap_or("").to_owned()),
                ),
            );
            body.insert(
                String::from("status"),
                Value::String(status.unwrap_or_else(|| {
                    api_string(&current, "status")
                        .unwrap_or("needsAction")
                        .to_owned()
                })),
            );
            if let Some(notes) = notes {
                body.insert(String::from("notes"), Value::String(notes));
            } else if let Some(notes) =
                api_string(&current, "notes").filter(|value| !value.is_empty())
            {
                body.insert(String::from("notes"), Value::String(notes.to_owned()));
            }
            if let Some(due) = due {
                body.insert(String::from("due"), Value::String(due));
            } else if let Some(due) = api_string(&current, "due").filter(|value| !value.is_empty())
            {
                body.insert(String::from("due"), Value::String(due.to_owned()));
            }
            let result = send(
                &session,
                "manage_task",
                Method::PUT,
                &url,
                &[],
                Some(&Value::Object(body)),
            )
            .await?;
            let mut response = format!(
                "Task Updated for {}:\n- Title: {}\n- ID: {}\n- Status: {}\n- Updated: {}",
                session.user_email,
                required_api_string(&result, "title")?,
                required_api_string(&result, "id")?,
                api_string(&result, "status").unwrap_or("N/A"),
                api_string(&result, "updated").unwrap_or("N/A")
            );
            append_present(&mut response, "Due Date", api_string(&result, "due"));
            append_present(&mut response, "Notes", api_string(&result, "notes"));
            append_present(&mut response, "Completed", api_string(&result, "completed"));
            Ok(response)
        }
        "delete" => {
            let task_id = task_id.expect("validated task_id");
            send(
                &session,
                "manage_task",
                Method::DELETE,
                &task_url(&task_list_id, &task_id),
                &[],
                None,
            )
            .await?;
            Ok(format!(
                "Task {task_id} has been deleted from task list {task_list_id} for {}.",
                session.user_email
            ))
        }
        "move" => {
            let task_id = task_id.expect("validated task_id");
            let mut query = Vec::new();
            push_nonempty(&mut query, "parent", parent.as_deref());
            push_nonempty(&mut query, "previous", previous.as_deref());
            push_nonempty(
                &mut query,
                "destinationTasklist",
                destination_task_list.as_deref(),
            );
            let result = send(
                &session,
                "manage_task",
                Method::POST,
                &format!("{}/move", task_url(&task_list_id, &task_id)),
                &query,
                None,
            )
            .await?;
            let mut response = format!(
                "Task Moved for {}:\n- Title: {}\n- ID: {}\n- Status: {}\n- Updated: {}",
                session.user_email,
                required_api_string(&result, "title")?,
                required_api_string(&result, "id")?,
                api_string(&result, "status").unwrap_or("N/A"),
                api_string(&result, "updated").unwrap_or("N/A")
            );
            append_present(
                &mut response,
                "Parent Task ID",
                api_string(&result, "parent"),
            );
            append_present(&mut response, "Position", api_string(&result, "position"));
            let mut details = Vec::new();
            if let Some(destination) = destination_task_list.filter(|value| !value.is_empty()) {
                details.push(format!("moved to task list {destination}"));
            }
            if let Some(parent) = parent.filter(|value| !value.is_empty()) {
                details.push(format!("made a subtask of {parent}"));
            }
            if let Some(previous) = previous.filter(|value| !value.is_empty()) {
                details.push(format!("positioned after {previous}"));
            }
            if !details.is_empty() {
                response.push_str(&format!("\n- Move Details: {}", details.join(", ")));
            }
            Ok(response)
        }
        _ => unreachable!(),
    }
}

async fn authenticated_session(
    arguments: &Map<String, Value>,
    required_scope: &str,
) -> Result<GoogleSession, String> {
    let supplied_email = optional_string(arguments, "user_google_email")?;
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

fn task_url(task_list_id: &str, task_id: &str) -> String {
    format!(
        "{TASKS_API}/lists/{}/tasks/{}",
        path_segment(task_list_id),
        path_segment(task_id)
    )
}

fn path_segment(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn push_nonempty(query: &mut Vec<(String, String)>, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        query.push((name.to_owned(), value.to_owned()));
    }
}

fn deny_restricted_action(action: &str, restrict_manage_actions: bool) -> Result<(), String> {
    if restrict_manage_actions && matches!(action, "delete" | "clear_completed") {
        return Err(format!(
            "The '{action}' action is not allowed under the current permission level."
        ));
    }
    Ok(())
}

fn validate_due(due: &str) -> Result<(), String> {
    let invalid = || {
        format!(
            "Invalid due date format. Expected RFC 3339 datetime (e.g., '2026-04-25T00:00:00Z'), got '{due}'"
        )
    };
    if !due.contains('T') || OffsetDateTime::parse(due, &Rfc3339).is_err() {
        return Err(invalid());
    }
    Ok(())
}

fn adjust_due_max(due_max: &str) -> String {
    let parsed = OffsetDateTime::parse(due_max, &Rfc3339).or_else(|_| {
        let normalized = if due_max.contains('T') {
            format!("{due_max}Z")
        } else {
            format!("{due_max}T00:00:00Z")
        };
        OffsetDateTime::parse(&normalized, &Rfc3339)
    });
    let Ok(parsed) = parsed else {
        return due_max.to_owned();
    };
    parsed
        .checked_add(Duration::days(1))
        .and_then(|adjusted| adjusted.format(&Rfc3339).ok())
        .unwrap_or_else(|| due_max.to_owned())
}

#[derive(Clone)]
struct TaskNode {
    title: Option<String>,
    status: Option<String>,
    due: Option<String>,
    notes: Option<String>,
    updated: Option<String>,
    completed: Option<String>,
    placeholder: bool,
}

struct StructuredTasks {
    roots: Vec<String>,
    nodes: HashMap<String, TaskNode>,
    children: HashMap<String, Vec<String>>,
    placeholder_count: usize,
}

fn structure_tasks(tasks: &[Value]) -> Result<StructuredTasks, String> {
    let mut nodes = HashMap::new();
    let mut positions = HashMap::new();
    for task in tasks {
        let id = required_api_string(task, "id")?.to_owned();
        nodes.insert(
            id.clone(),
            TaskNode {
                title: api_string(task, "title").map(str::to_owned),
                status: api_string(task, "status").map(str::to_owned),
                due: api_string(task, "due").map(str::to_owned),
                notes: api_string(task, "notes").map(str::to_owned),
                updated: api_string(task, "updated").map(str::to_owned),
                completed: api_string(task, "completed").map(str::to_owned),
                placeholder: false,
            },
        );
        if let Some(position) = api_string(task, "position") {
            positions.insert(
                id,
                position
                    .parse::<u128>()
                    .map_err(|error| format!("Unexpected error: {error}."))?,
            );
        }
    }

    let mut roots = Vec::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for task in tasks {
        let id = required_api_string(task, "id")?.to_owned();
        match api_string(task, "parent").filter(|parent| !parent.is_empty()) {
            None => roots.push(id),
            Some(parent) if nodes.contains_key(parent) => {
                children.entry(parent.to_owned()).or_default().push(id);
            }
            Some(parent) => {
                nodes.insert(
                    parent.to_owned(),
                    TaskNode {
                        title: None,
                        status: None,
                        due: None,
                        notes: None,
                        updated: None,
                        completed: None,
                        placeholder: true,
                    },
                );
                roots.push(parent.to_owned());
                children.entry(parent.to_owned()).or_default().push(id);
            }
        }
    }

    roots.sort_by_key(|id| positions.get(id).copied().unwrap_or(u128::MAX));
    for child_ids in children.values_mut() {
        child_ids.sort_by_key(|id| positions.get(id).copied().unwrap_or(u128::MAX));
    }
    let placeholder_count = roots
        .iter()
        .filter(|id| nodes.get(*id).is_some_and(|node| node.placeholder))
        .count();
    Ok(StructuredTasks {
        roots,
        nodes,
        children,
        placeholder_count,
    })
}

fn serialize_tasks(
    ids: &[String],
    nodes: &HashMap<String, TaskNode>,
    children: &HashMap<String, Vec<String>>,
    level: usize,
    response: &mut String,
) {
    for id in ids {
        let Some(task) = nodes.get(id) else {
            continue;
        };
        let indent = "  ".repeat(level);
        let bullet = if level == 0 { '-' } else { '*' };
        let title = task.title.as_deref().unwrap_or(if task.placeholder {
            "Unknown parent"
        } else {
            "Untitled"
        });
        response.push_str(&format!("{indent}{bullet} {title} (ID: {id})\n"));
        response.push_str(&format!(
            "{indent}  Status: {}\n",
            task.status.as_deref().unwrap_or("N/A")
        ));
        if let Some(due) = task.due.as_deref().filter(|value| !value.is_empty()) {
            response.push_str(&format!("{indent}  Due: {due}\n"));
        }
        if let Some(notes) = task.notes.as_deref().filter(|value| !value.is_empty()) {
            let shortened = notes.chars().take(100).collect::<String>();
            let suffix = if notes.chars().count() > 100 {
                "..."
            } else {
                ""
            };
            response.push_str(&format!("{indent}  Notes: {shortened}{suffix}\n"));
        }
        if let Some(completed) = task.completed.as_deref().filter(|value| !value.is_empty()) {
            response.push_str(&format!("{indent}  Completed: {completed}\n"));
        }
        response.push_str(&format!(
            "{indent}  Updated: {}\n\n",
            task.updated.as_deref().unwrap_or("N/A")
        ));
        if let Some(child_ids) = children.get(id) {
            serialize_tasks(child_ids, nodes, children, level + 1, response);
        }
    }
}

fn append_present(response: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        response.push_str(&format!("\n- {label}: {value}"));
    }
}

fn required_api_string<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    api_string(value, name)
        .ok_or_else(|| format!("Unexpected error: Google Tasks response is missing '{name}'."))
}

fn api_string<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

fn required_string(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    optional_string(arguments, name)?.ok_or_else(|| format!("{name} is required"))
}

fn optional_string(arguments: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
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

fn integer(arguments: &Map<String, Value>, name: &str, default: i64) -> Result<i64, String> {
    match argument(arguments, name) {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| format!("{name} must be an integer")),
    }
}

fn boolean(arguments: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    match argument(arguments, name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{name} must be a boolean")),
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
            "max_results" if !value.is_i64() => {
                return Err(String::from("max_results must be an integer"));
            }
            "show_completed" | "show_deleted" | "show_hidden" | "show_assigned"
                if !value.is_boolean() =>
            {
                return Err(format!("{canonical} must be a boolean"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjusts_due_max_and_validates_due() {
        assert_eq!(
            adjust_due_max("2026-04-25T00:00:00Z"),
            "2026-04-26T00:00:00Z"
        );
        assert_eq!(adjust_due_max("2026-04-25"), "2026-04-26T00:00:00Z");
        assert!(validate_due("2026-04-25T00:00:00Z").is_ok());
        assert!(validate_due("2026-04-25").is_err());
        assert!(validate_due("2026-04-25T00:00:00").is_err());
    }

    #[test]
    fn encodes_path_segments() {
        assert_eq!(path_segment("list/id +"), "list%2Fid%20%2B");
    }

    #[test]
    fn structures_and_serializes_tasks() {
        let tasks = vec![
            json!({"id":"child","title":"Child","parent":"parent","position":"2","status":"needsAction","updated":"now"}),
            json!({"id":"parent","title":"Parent","position":"1","status":"needsAction","updated":"now"}),
        ];
        let structured = structure_tasks(&tasks).expect("structure tasks");
        let mut output = String::new();
        serialize_tasks(
            &structured.roots,
            &structured.nodes,
            &structured.children,
            0,
            &mut output,
        );
        assert!(output.starts_with("- Parent (ID: parent)"));
        assert!(output.contains("  * Child (ID: child)"));
    }
}
