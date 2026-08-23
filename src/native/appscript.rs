use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::auth::GoogleSession;

const SCRIPT_API: &str = "https://script.googleapis.com/v1";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
const DRIVE_RO: &str = "https://www.googleapis.com/auth/drive.readonly";
const DRIVE: &str = "https://www.googleapis.com/auth/drive";
const PROJECTS_RO: &str = "https://www.googleapis.com/auth/script.projects.readonly";
const PROJECTS: &str = "https://www.googleapis.com/auth/script.projects";
const DEPLOYMENTS_RO: &str = "https://www.googleapis.com/auth/script.deployments.readonly";
const DEPLOYMENTS: &str = "https://www.googleapis.com/auth/script.deployments";
const RUN: &str = "https://www.googleapis.com/auth/script.external_request";
const PROCESSES_RO: &str = "https://www.googleapis.com/auth/script.processes";
const METRICS_RO: &str = "https://www.googleapis.com/auth/script.metrics";
const MAX_FILES: usize = 100;
const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;

static UPDATE_LOCKS: OnceLock<StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>> = OnceLock::new();

#[derive(Clone, Copy)]
enum Kind {
    String,
    NullableString,
    Integer,
    NullableInteger,
    Boolean,
    NullableArray,
    ObjectArray,
}
const P: (&str, Kind) = ("user_google_email", Kind::String);
const LIST_PROJECT: &[(&str, Kind)] = &[
    P,
    ("page_size", Kind::Integer),
    ("page_token", Kind::NullableString),
];
const SCRIPT: &[(&str, Kind)] = &[P, ("script_id", Kind::String)];
const CONTENT: &[(&str, Kind)] = &[P, ("script_id", Kind::String), ("file_name", Kind::String)];
const CREATE: &[(&str, Kind)] = &[
    P,
    ("title", Kind::String),
    ("parent_id", Kind::NullableString),
];
const UPDATE: &[(&str, Kind)] = &[
    P,
    ("script_id", Kind::String),
    ("files", Kind::ObjectArray),
    ("merge", Kind::Boolean),
];
const RUN_ARGS: &[(&str, Kind)] = &[
    P,
    ("script_id", Kind::String),
    ("function_name", Kind::String),
    ("parameters", Kind::NullableArray),
    ("dev_mode", Kind::Boolean),
];
const DEPLOY: &[(&str, Kind)] = &[
    P,
    ("action", Kind::String),
    ("script_id", Kind::String),
    ("deployment_id", Kind::NullableString),
    ("description", Kind::NullableString),
    ("version_description", Kind::NullableString),
    ("version_number", Kind::NullableInteger),
];
const PROCESS: &[(&str, Kind)] = &[
    P,
    ("page_size", Kind::Integer),
    ("script_id", Kind::NullableString),
];
const CREATE_VERSION: &[(&str, Kind)] = &[
    P,
    ("script_id", Kind::String),
    ("description", Kind::NullableString),
];
const GET_VERSION: &[(&str, Kind)] = &[
    P,
    ("script_id", Kind::String),
    ("version_number", Kind::Integer),
];
const METRICS: &[(&str, Kind)] = &[
    P,
    ("script_id", Kind::String),
    ("metrics_granularity", Kind::String),
];
const TRIGGER: &[(&str, Kind)] = &[
    ("trigger_type", Kind::String),
    ("function_name", Kind::String),
    ("schedule", Kind::String),
];

pub async fn dispatch(name: &str, a: &Map<String, Value>) -> Result<String, String> {
    match name {
        "list_script_projects" => list_projects(a).await,
        "get_script_project" => get_project(a).await,
        "get_script_content" => get_content(a).await,
        "create_script_project" => create_project(a).await,
        "update_script_content" => update_content(a).await,
        "run_script_function" => run_function(a).await,
        "manage_deployment" => manage_deployment(a).await,
        "list_deployments" => list_deployments(a).await,
        "list_script_processes" => list_processes(a).await,
        "delete_script_project" => delete_project(a).await,
        "list_versions" => list_versions(a).await,
        "create_version" => create_version(a).await,
        "get_version" => get_version(a).await,
        "get_script_metrics" => get_metrics(a).await,
        "generate_trigger_code" => generate_trigger(a),
        _ => Err(format!("Unsupported native Apps Script tool: {name}")),
    }
}

async fn list_projects(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, LIST_PROJECT)?;
    let mut q = vec![
        (
            "q".into(),
            "mimeType='application/vnd.google-apps.script' and trashed=false".into(),
        ),
        ("pageSize".into(), integer(a, "page_size", 50)?.to_string()),
        (
            "fields".into(),
            "nextPageToken, files(id, name, createdTime, modifiedTime)".into(),
        ),
        ("orderBy".into(), "modifiedTime desc".into()),
    ];
    if let Some(v) = optional_string(a, "page_token")? {
        q.push(("pageToken".into(), v));
    }
    let s = session(a, DRIVE_RO, Some(DRIVE)).await?;
    let r = send(
        &s,
        "list_script_projects",
        Method::GET,
        &format!("{DRIVE_API}/files"),
        &q,
        None,
    )
    .await?;
    let files = objects(&r, "files");
    if files.is_empty() {
        return Ok("No Apps Script projects found.".into());
    }
    let mut out = vec![format!("Found {} Apps Script projects:", files.len())];
    for f in &files {
        out.push(format!(
            "- {} (ID: {}) Created: {} Modified: {}",
            field(f, "name", "Untitled"),
            field(f, "id", "Unknown ID"),
            field(f, "createdTime", "Unknown"),
            field(f, "modifiedTime", "Unknown")
        ));
    }
    if let Some(v) = r.get("nextPageToken").and_then(Value::as_str) {
        out.push(format!("\nNext page token: {v}"));
    }
    Ok(out.join("\n"))
}

async fn get_project(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SCRIPT)?;
    let id = required_string(a, "script_id")?;
    let s = session(a, PROJECTS_RO, Some(PROJECTS)).await?;
    let root = format!("{SCRIPT_API}/projects/{}", segment(&id));
    let content_url = format!("{root}/content");
    let (meta, content) = tokio::join!(
        send(&s, "get_script_project", Method::GET, &root, &[], None),
        send(
            &s,
            "get_script_project",
            Method::GET,
            &content_url,
            &[],
            None
        )
    );
    let (meta, content) = (meta?, content?);
    let creator = meta
        .get("creator")
        .and_then(Value::as_object)
        .map_or("Unknown", |v| field(v, "email", "Unknown"));
    let mut out = vec![
        format!(
            "Project: {} (ID: {})",
            vfield(&meta, "title", "Untitled"),
            vfield(&meta, "scriptId", "Unknown")
        ),
        format!("Creator: {creator}"),
        format!("Created: {}", vfield(&meta, "createTime", "Unknown")),
        format!("Modified: {}", vfield(&meta, "updateTime", "Unknown")),
        "".into(),
        "Files:".into(),
    ];
    for (i, f) in objects(&content, "files").iter().enumerate() {
        out.push(format!(
            "{}. {} ({})",
            i + 1,
            field(f, "name", "Untitled"),
            field(f, "type", "Unknown")
        ));
        let source = field(f, "source", "");
        if !source.is_empty() {
            let (p, more) = first_chars(source, 200);
            out.push(format!("   {p}{}", if more { "..." } else { "" }));
            out.push("".into());
        }
    }
    Ok(out.join("\n"))
}

async fn get_content(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, CONTENT)?;
    let id = required_string(a, "script_id")?;
    let name = required_string(a, "file_name")?;
    let s = session(a, PROJECTS_RO, Some(PROJECTS)).await?;
    let r = send(
        &s,
        "get_script_content",
        Method::GET,
        &format!("{SCRIPT_API}/projects/{}/content", segment(&id)),
        &[],
        None,
    )
    .await?;
    let files = objects(&r, "files");
    let Some(f) = files
        .iter()
        .find(|f| f.get("name").and_then(Value::as_str) == Some(&name))
    else {
        return Ok(format!("File '{name}' not found in project {id}"));
    };
    Ok(format!(
        "File: {name} ({})\n\n{}",
        field(f, "type", "Unknown"),
        field(f, "source", "")
    ))
}

async fn create_project(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, CREATE)?;
    let title = required_string(a, "title")?;
    let mut body = json!({"title": title});
    if let Some(parent) = optional_string(a, "parent_id")? {
        body["parentId"] = parent.into();
    }
    let s = session(a, PROJECTS, None).await?;
    let r = send(
        &s,
        "create_script_project",
        Method::POST,
        &format!("{SCRIPT_API}/projects"),
        &[],
        Some(&body),
    )
    .await?;
    let id = vfield(&r, "scriptId", "Unknown");
    Ok(format!(
        "Created Apps Script project: {title}\nScript ID: {id}\nEdit URL: https://script.google.com/d/{id}/edit"
    ))
}

async fn update_content(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, UPDATE)?;
    let id = required_string(a, "script_id")?;
    let mut files: Vec<Map<String, Value>> = object_array(a, "files")?
        .iter()
        .map(normalize_file)
        .collect();
    let merge = boolean(a, "merge", true)?;
    validate_sizes(&files)?;
    let s = session(a, PROJECTS, None).await?;
    let lock = update_lock(&id)?;
    let _guard = lock.lock().await;
    let url = format!("{SCRIPT_API}/projects/{}/content", segment(&id));
    if merge {
        let current = send(&s, "update_script_content", Method::GET, &url, &[], None).await?;
        files = merge_files(&objects(&current, "files"), &files)?;
        validate_sizes(&files)?;
    }
    let r = send(
        &s,
        "update_script_content",
        Method::PUT,
        &url,
        &[],
        Some(&json!({"files": files})),
    )
    .await?;
    let mode = if merge {
        "merged into project"
    } else {
        "replaced entire project"
    };
    let mut out = vec![
        format!("Updated script project: {id} ({mode})"),
        "".into(),
        "Files in project after update:".into(),
    ];
    let updated_files = objects(&r, "files");
    for f in &updated_files {
        out.push(format!(
            "- {} ({})",
            field(f, "name", "Untitled"),
            field(f, "type", "Unknown")
        ));
    }
    Ok(out.join("\n"))
}

async fn run_function(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, RUN_ARGS)?;
    let id = required_string(a, "script_id")?;
    let function = required_string(a, "function_name")?;
    let mut body = json!({"function": function, "devMode": boolean(a, "dev_mode", false)?});
    if let Some(p) = optional_array(a, "parameters")?.filter(|v| !v.is_empty()) {
        body["parameters"] = Value::Array(p);
    }
    let s = match session(a, RUN, None).await {
        Ok(v) => v,
        Err(e) => return Ok(run_error(&function, &e)),
    };
    let r = match send(
        &s,
        "run_script_function",
        Method::POST,
        &format!("{SCRIPT_API}/scripts/{}:run", segment(&id)),
        &[],
        Some(&body),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return Ok(run_error(&function, &e)),
    };
    if let Some(e) = r.get("error") {
        return Ok(run_error(
            &function,
            &e.get("message")
                .map(display)
                .unwrap_or_else(|| "Unknown error".into()),
        ));
    }
    let result = r
        .pointer("/response/result")
        .map(display)
        .unwrap_or_else(|| "None".into());
    Ok(format!(
        "Execution successful\nFunction: {function}\nResult: {result}"
    ))
}

async fn manage_deployment(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, DEPLOY)?;
    let action = required_string(a, "action")?.trim().to_ascii_lowercase();
    let id = required_string(a, "script_id")?;
    let dep = optional_string(a, "deployment_id")?;
    let desc = optional_string(a, "description")?;
    let version_desc = optional_string(a, "version_description")?;
    let version = optional_integer(a, "version_number")?;
    match action.as_str() {
        "create" if desc.as_deref().is_none_or(|v| v.trim().is_empty()) => {
            return Err("description is required for create action".into());
        }
        "update" if dep.as_deref().is_none_or(str::is_empty) => {
            return Err("deployment_id is required for update action".into());
        }
        "update" if desc.as_deref().is_none_or(|v| v.trim().is_empty()) && version.is_none() => {
            return Err("description or version_number is required for update action".into());
        }
        "delete" if dep.as_deref().is_none_or(str::is_empty) => {
            return Err("deployment_id is required for delete action".into());
        }
        "create" | "update" | "delete" => {}
        _ => {
            return Err(format!(
                "Invalid action '{action}'. Must be 'create', 'update', or 'delete'."
            ));
        }
    }
    let s = session(a, DEPLOYMENTS, None).await?;
    let root = format!("{SCRIPT_API}/projects/{}", segment(&id));
    match action.as_str() {
        "create" => {
            let desc = desc.expect("validated description");
            let vd = version_desc
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| desc.clone());
            let ver = send(
                &s,
                "manage_deployment",
                Method::POST,
                &format!("{root}/versions"),
                &[],
                Some(&json!({"description": vd})),
            )
            .await?;
            let number = ver.get("versionNumber").cloned().unwrap_or(Value::Null);
            let result = send(
                &s,
                "manage_deployment",
                Method::POST,
                &format!("{root}/deployments"),
                &[],
                Some(&json!({"versionNumber": number, "description": desc})),
            )
            .await?;
            Ok(format!(
                "Created deployment for script: {id}\nDeployment ID: {}\nVersion: {}\nDescription: {desc}",
                vfield(&result, "deploymentId", "Unknown"),
                display(&number)
            ))
        }
        "update" => {
            let dep = dep.expect("validated deployment_id");
            let mut cfg = Map::new();
            cfg.insert("scriptId".into(), id.clone().into());
            if let Some(v) = version {
                cfg.insert("versionNumber".into(), v.into());
            }
            if let Some(v) = desc.filter(|v| !v.is_empty()) {
                cfg.insert("description".into(), v.into());
            }
            let result = send(
                &s,
                "manage_deployment",
                Method::PUT,
                &format!("{root}/deployments/{}", segment(&dep)),
                &[],
                Some(&json!({"deploymentConfig": cfg})),
            )
            .await?;
            let cfg = result.get("deploymentConfig").and_then(Value::as_object);
            let v = cfg
                .and_then(|v| v.get("versionNumber"))
                .map(display)
                .or_else(|| version.map(|v| v.to_string()))
                .unwrap_or_else(|| "unchanged".into());
            let d = cfg
                .and_then(|v| v.get("description"))
                .map(display)
                .or_else(|| result.get("description").map(display))
                .unwrap_or_else(|| "No description".into());
            Ok(format!(
                "Updated deployment: {dep}\nScript: {id}\nVersion: {v}\nDescription: {d}"
            ))
        }
        "delete" => {
            let dep = dep.expect("validated deployment_id");
            send(
                &s,
                "manage_deployment",
                Method::DELETE,
                &format!("{root}/deployments/{}", segment(&dep)),
                &[],
                None,
            )
            .await?;
            Ok(format!("Deleted deployment: {dep} from script: {id}"))
        }
        _ => unreachable!(),
    }
}

async fn list_deployments(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SCRIPT)?;
    let id = required_string(a, "script_id")?;
    let s = session(a, DEPLOYMENTS_RO, Some(DEPLOYMENTS)).await?;
    let r = send(
        &s,
        "list_deployments",
        Method::GET,
        &format!("{SCRIPT_API}/projects/{}/deployments", segment(&id)),
        &[],
        None,
    )
    .await?;
    let values = objects(&r, "deployments");
    if values.is_empty() {
        return Ok(format!("No deployments found for script: {id}"));
    }
    let mut out = vec![format!("Deployments for script: {id}"), "".into()];
    for (i, v) in values.iter().enumerate() {
        out.push(format!(
            "{}. {} ({})",
            i + 1,
            field(v, "description", "No description"),
            field(v, "deploymentId", "Unknown")
        ));
        out.push(format!("   Updated: {}", field(v, "updateTime", "Unknown")));
        out.push("".into());
    }
    Ok(out.join("\n"))
}

async fn list_processes(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, PROCESS)?;
    let mut q = vec![("pageSize".into(), integer(a, "page_size", 50)?.to_string())];
    if let Some(v) = optional_string(a, "script_id")? {
        q.push(("userProcessFilter.scriptId".into(), v));
    }
    let s = session(a, PROCESSES_RO, None).await?;
    let r = send(
        &s,
        "list_script_processes",
        Method::GET,
        &format!("{SCRIPT_API}/processes"),
        &q,
        None,
    )
    .await?;
    let values = objects(&r, "processes");
    if values.is_empty() {
        return Ok("No recent script executions found.".into());
    }
    let mut out = vec!["Recent script executions:".into(), "".into()];
    for (i, v) in values.iter().enumerate() {
        out.extend([
            format!("{}. {}", i + 1, field(v, "functionName", "Unknown")),
            format!("   Status: {}", field(v, "processStatus", "Unknown")),
            format!("   Started: {}", field(v, "startTime", "Unknown")),
            format!("   Duration: {}", field(v, "duration", "Unknown")),
            "".into(),
        ]);
    }
    Ok(out.join("\n"))
}

async fn delete_project(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SCRIPT)?;
    let id = required_string(a, "script_id")?;
    let s = session(a, DRIVE, None).await?;
    send(
        &s,
        "delete_script_project",
        Method::DELETE,
        &format!("{DRIVE_API}/files/{}", segment(&id)),
        &[],
        None,
    )
    .await?;
    Ok(format!("Deleted Apps Script project: {id}"))
}

async fn list_versions(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SCRIPT)?;
    let id = required_string(a, "script_id")?;
    let s = session(a, PROJECTS_RO, Some(PROJECTS)).await?;
    let r = send(
        &s,
        "list_versions",
        Method::GET,
        &format!("{SCRIPT_API}/projects/{}/versions", segment(&id)),
        &[],
        None,
    )
    .await?;
    let values = objects(&r, "versions");
    if values.is_empty() {
        return Ok(format!("No versions found for script: {id}"));
    }
    let mut out = vec![format!("Versions for script: {id}"), "".into()];
    for v in &values {
        out.extend([
            format!(
                "Version {}: {}",
                map_field(v, "versionNumber", "Unknown"),
                field(v, "description", "No description")
            ),
            format!("   Created: {}", field(v, "createTime", "Unknown")),
            "".into(),
        ]);
    }
    Ok(out.join("\n"))
}

async fn create_version(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, CREATE_VERSION)?;
    let id = required_string(a, "script_id")?;
    let desc = optional_string(a, "description")?;
    let s = session(a, PROJECTS, None).await?;
    let body = desc
        .as_ref()
        .filter(|v| !v.is_empty())
        .map_or_else(|| json!({}), |v| json!({"description": v}));
    let r = send(
        &s,
        "create_version",
        Method::POST,
        &format!("{SCRIPT_API}/projects/{}/versions", segment(&id)),
        &[],
        Some(&body),
    )
    .await?;
    Ok(format!(
        "Created version {} for script: {id}\nDescription: {}\nCreated: {}",
        vfield(&r, "versionNumber", "Unknown"),
        desc.filter(|v| !v.is_empty())
            .unwrap_or_else(|| "No description".into()),
        vfield(&r, "createTime", "Unknown")
    ))
}

async fn get_version(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, GET_VERSION)?;
    let id = required_string(a, "script_id")?;
    let number = required_integer(a, "version_number")?;
    let s = session(a, PROJECTS_RO, Some(PROJECTS)).await?;
    let r = send(
        &s,
        "get_version",
        Method::GET,
        &format!("{SCRIPT_API}/projects/{}/versions/{number}", segment(&id)),
        &[],
        None,
    )
    .await?;
    Ok(format!(
        "Version {} of script: {id}\nDescription: {}\nCreated: {}",
        vfield(&r, "versionNumber", "Unknown"),
        vfield(&r, "description", "No description"),
        vfield(&r, "createTime", "Unknown")
    ))
}

async fn get_metrics(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, METRICS)?;
    let id = required_string(a, "script_id")?;
    let granularity = string(a, "metrics_granularity", "DAILY")?;
    let s = session(a, METRICS_RO, None).await?;
    let r = send(
        &s,
        "get_script_metrics",
        Method::GET,
        &format!("{SCRIPT_API}/projects/{}/metrics", segment(&id)),
        &[("metricsGranularity".into(), granularity.clone())],
        None,
    )
    .await?;
    let active = objects(&r, "activeUsers");
    let total = objects(&r, "totalExecutions");
    let failed = objects(&r, "failedExecutions");
    let mut out = vec![
        format!("Metrics for script: {id}"),
        format!("Granularity: {granularity}"),
        "".into(),
    ];
    metric_lines(&mut out, "Active Users:", &active, "users");
    metric_lines(&mut out, "Total Executions:", &total, "executions");
    metric_lines(&mut out, "Failed Executions:", &failed, "failures");
    if active.is_empty() && total.is_empty() && failed.is_empty() {
        out.push("No metrics data available for this script.".into());
    }
    Ok(out.join("\n"))
}

fn generate_trigger(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, TRIGGER)?;
    let kind = required_string(a, "trigger_type")?;
    let function = required_string(a, "function_name")?;
    let schedule = string(a, "schedule", "")?;
    validate_identifier(&function)?;
    let code = match kind.as_str() {
        "on_open" => {
            empty_schedule(&kind, &schedule)?;
            format!(
                "// Simple trigger - just rename your function to 'onOpen'\n// This runs automatically when the document is opened\nfunction onOpen(e) {{\n  {function}();\n}}"
            )
        }
        "on_edit" => {
            empty_schedule(&kind, &schedule)?;
            format!(
                "// Simple trigger - just rename your function to 'onEdit'\n// This runs automatically when a user edits the spreadsheet\nfunction onEdit(e) {{\n  {function}();\n}}"
            )
        }
        "time_minutes" => {
            let v = if schedule.is_empty() { "5" } else { &schedule };
            if !["1", "5", "10", "15", "30"].contains(&v) {
                return Err("schedule for time_minutes must be one of: 1, 5, 10, 15, 30".into());
            }
            timed(
                &function,
                "Time",
                &format!("runs every {v} minutes"),
                &format!(".everyMinutes({v})"),
                &format!("will run every {v} minutes"),
            )
        }
        "time_hours" => {
            let v = if schedule.is_empty() { "1" } else { &schedule };
            if !["1", "2", "4", "6", "8", "12"].contains(&v) {
                return Err("schedule for time_hours must be one of: 1, 2, 4, 6, 8, 12".into());
            }
            timed(
                &function,
                "Time",
                &format!("runs every {v} hour(s)"),
                &format!(".everyHours({v})"),
                &format!("will run every {v} hour(s)"),
            )
        }
        "time_daily" => {
            let h = hour(if schedule.is_empty() { "9" } else { &schedule })?;
            timed(
                &function,
                "Daily",
                &format!("runs daily at {h}:00"),
                &format!(".atHour({h})\n    .everyDays(1)"),
                &format!("will run daily at {h}:00"),
            )
        }
        "time_weekly" => {
            let day = if schedule.is_empty() {
                "MONDAY".into()
            } else {
                schedule.to_ascii_uppercase()
            };
            if ![
                "MONDAY",
                "TUESDAY",
                "WEDNESDAY",
                "THURSDAY",
                "FRIDAY",
                "SATURDAY",
                "SUNDAY",
            ]
            .contains(&day.as_str())
            {
                return Err(
                    "schedule for time_weekly must be a weekday from MONDAY through SUNDAY".into(),
                );
            }
            timed(
                &function,
                "Weekly",
                &format!("runs weekly on {day}"),
                &format!(".onWeekDay(ScriptApp.WeekDay.{day})\n    .atHour(9)"),
                &format!("will run every {day} at 9:00"),
            )
        }
        "on_form_submit" => {
            empty_schedule(&kind, &schedule)?;
            event_trigger(
                &function,
                "FormSubmit",
                "// This must be run from a script BOUND to the Google Form",
                "runs when form is submitted",
                ".forForm(FormApp.getActiveForm())\n    .onFormSubmit()",
                "will run on form submit",
            )
        }
        "on_change" => {
            empty_schedule(&kind, &schedule)?;
            event_trigger(
                &function,
                "Change",
                "// This must be run from a script BOUND to a Google Sheet",
                "runs when spreadsheet changes",
                ".forSpreadsheet(SpreadsheetApp.getActive())\n    .onChange()",
                "will run on spreadsheet change",
            )
        }
        _ => {
            return Ok(format!(
                "Unknown trigger type: {kind}\n\nValid types: time_minutes, time_hours, time_daily, time_weekly, on_open, on_edit, on_form_submit, on_change"
            ));
        }
    };
    let instructions = if matches!(kind.as_str(), "on_open" | "on_edit") {
        "SIMPLE TRIGGER\n==================================================\n\nAdd this code to your script. Simple triggers run automatically\nwhen the event occurs - no setup function needed.\n\nNote: Simple triggers have limitations:\n- Cannot access services that require authorization\n- Cannot run longer than 30 seconds\n- Cannot make external HTTP requests\n\nFor more capabilities, use an installable trigger instead.\n\nCODE TO ADD:\n--------------------------------------------------".into()
    } else if kind.starts_with("on_") {
        format!(
            "INSTALLABLE TRIGGER\n==================================================\n\n1. Add this code to your script\n2. Run the setup function once: createFormSubmitTrigger_{function}() or similar\n3. The trigger will then run automatically\n\nCODE TO ADD:\n--------------------------------------------------"
        )
    } else {
        "INSTALLABLE TRIGGER\n==================================================\n\n1. Add this code to your script using update_script_content\n2. Run the setup function ONCE (manually in Apps Script editor or via run_script_function)\n3. The trigger will then run automatically on schedule\n\nTo check installed triggers: Apps Script editor > Triggers (clock icon)\n\nCODE TO ADD:\n--------------------------------------------------".into()
    };
    Ok(format!("{instructions}\n\n{code}"))
}

fn timed(f: &str, setup: &str, comment: &str, builder: &str, log: &str) -> String {
    format!(
        "// Run this function ONCE to install the trigger\nfunction create{setup}Trigger_{f}() {{\n  // Delete existing triggers for this function first\n  const triggers = ScriptApp.getProjectTriggers();\n  triggers.forEach(trigger => {{\n    if (trigger.getHandlerFunction() === '{f}') {{\n      ScriptApp.deleteTrigger(trigger);\n    }}\n  }});\n\n  // Create new trigger - {comment}\n  ScriptApp.newTrigger('{f}')\n    .timeBased()\n    {builder}\n    .create();\n\n  Logger.log('Trigger created: {f} {log}');\n}}"
    )
}
fn event_trigger(
    f: &str,
    setup: &str,
    binding: &str,
    comment: &str,
    builder: &str,
    log: &str,
) -> String {
    format!(
        "// Run this function ONCE to install the trigger\n{binding}\nfunction create{setup}Trigger_{f}() {{\n  // Delete existing triggers for this function first\n  const triggers = ScriptApp.getProjectTriggers();\n  triggers.forEach(trigger => {{\n    if (trigger.getHandlerFunction() === '{f}') {{\n      ScriptApp.deleteTrigger(trigger);\n    }}\n  }});\n\n  // Create new trigger - {comment}\n  ScriptApp.newTrigger('{f}')\n    {builder}\n    .create();\n\n  Logger.log('Trigger created: {f} {log}');\n}}"
    )
}

fn validate_identifier(v: &str) -> Result<(), String> {
    const RESERVED: &[&str] = &[
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ];
    let mut chars = v.chars();
    let Some(first) = chars.next() else {
        return Err("function_name must be a valid JavaScript identifier".into());
    };
    if !(first.is_ascii_alphabetic() || matches!(first, '_' | '$'))
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
        || RESERVED.contains(&v)
    {
        return Err("function_name must be a valid JavaScript identifier".into());
    }
    Ok(())
}
fn hour(v: &str) -> Result<String, String> {
    if v.is_empty() || v.len() > 2 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err("schedule for time_daily must be an hour from 0 through 23".into());
    }
    let n: u8 = v
        .parse()
        .map_err(|_| "schedule for time_daily must be an hour from 0 through 23".to_string())?;
    if n > 23 {
        return Err("schedule for time_daily must be an hour from 0 through 23".into());
    }
    Ok(n.to_string())
}
fn empty_schedule(kind: &str, v: &str) -> Result<(), String> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(format!("schedule must be empty for {kind}"))
    }
}

fn normalize_file(f: &Map<String, Value>) -> Map<String, Value> {
    ["name", "type", "source"]
        .into_iter()
        .filter_map(|k| {
            f.get(k)
                .filter(|v| !v.is_null())
                .cloned()
                .map(|v| (k.into(), v))
        })
        .collect()
}
fn merge_files(
    existing: &[Map<String, Value>],
    updates: &[Map<String, Value>],
) -> Result<Vec<Map<String, Value>>, String> {
    let mut out: Vec<_> = existing
        .iter()
        .filter(|f| {
            f.get("name")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        })
        .map(normalize_file)
        .collect();
    for (i, update) in updates.iter().enumerate() {
        let Some(name) = update
            .get("name")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        else {
            return Err(format!("File at index {i} is missing a non-empty 'name'."));
        };
        let file_type = update.get("type").and_then(Value::as_str);
        if let Some(t) = file_type {
            if !["SERVER_JS", "HTML", "JSON"].contains(&t) {
                return Err(format!(
                    "File '{name}' has unsupported type '{t}'; it must be one of SERVER_JS, HTML, or JSON."
                ));
            }
            if t == "JSON" && name != "appsscript" {
                return Err(format!(
                    "JSON file '{name}' must use the manifest name 'appsscript'."
                ));
            }
        }
        let pos = if let Some(t) = file_type {
            out.iter().position(|f| {
                f.get("name").and_then(Value::as_str) == Some(name)
                    && f.get("type").and_then(Value::as_str) == Some(t)
            })
        } else {
            let p: Vec<_> = out
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    (f.get("name").and_then(Value::as_str) == Some(name)).then_some(i)
                })
                .collect();
            if p.len() != 1 {
                return Err(format!(
                    "File '{name}' is missing 'type'; it must be one of SERVER_JS, HTML, or JSON because the existing project does not identify a single file with that name."
                ));
            }
            p.first().copied()
        };
        if let Some(p) = pos {
            for (k, v) in update {
                out[p].insert(k.clone(), v.clone());
            }
        } else {
            out.push(update.clone());
        }
    }
    Ok(out)
}
fn validate_sizes(files: &[Map<String, Value>]) -> Result<(), String> {
    if files.len() > MAX_FILES {
        return Err(format!(
            "Script project exceeds the {MAX_FILES} file limit ({} files).",
            files.len()
        ));
    }
    let mut total = 0usize;
    for f in files {
        let n = field(f, "source", "").len();
        if n > MAX_FILE_BYTES {
            return Err(format!(
                "Script file '{}' exceeds the {MAX_FILE_BYTES} byte limit ({n} bytes).",
                field(f, "name", "Untitled")
            ));
        }
        total = total
            .checked_add(n)
            .ok_or_else(|| "Script project source exceeds the total byte limit".to_string())?;
    }
    if total > MAX_TOTAL_BYTES {
        return Err(format!(
            "Script project source exceeds the {MAX_TOTAL_BYTES} byte total limit ({total} bytes)."
        ));
    }
    Ok(())
}
fn update_lock(id: &str) -> Result<Arc<AsyncMutex<()>>, String> {
    let locks = UPDATE_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| "Apps Script update lock is unavailable".to_string())?;
    if let Some(v) = locks.get(id).and_then(Weak::upgrade) {
        return Ok(v);
    }
    locks.retain(|_, v| v.strong_count() > 0);
    let v = Arc::new(AsyncMutex::new(()));
    locks.insert(id.into(), Arc::downgrade(&v));
    Ok(v)
}

async fn session(
    a: &Map<String, Value>,
    scope: &str,
    broad: Option<&str>,
) -> Result<GoogleSession, String> {
    let supplied = optional_string(a, "user_google_email")?;
    let s = match GoogleSession::authenticate(&[scope]).await {
        Ok(v) => v,
        Err(e) if broad.is_some() && e.contains("do not include the required Google scopes") => {
            GoogleSession::authenticate(&[broad.expect("checked broad scope")]).await?
        }
        Err(e) => return Err(e),
    };
    if supplied.is_some_and(|v| !v.eq_ignore_ascii_case(&s.user_email)) {
        return Err("user_google_email does not match the configured USER_GOOGLE_EMAIL".into());
    }
    Ok(s)
}
async fn send(
    s: &GoogleSession,
    tool: &str,
    method: Method,
    url: &str,
    query: &[(String, String)],
    body: Option<&Value>,
) -> Result<Value, String> {
    let mut r = s.request(method, url).query(query);
    if let Some(v) = body {
        r = r.json(v);
    }
    s.send_json(tool, r).await
}

fn validate(a: &Map<String, Value>, allowed: &[(&str, Kind)]) -> Result<(), String> {
    for (key, value) in a {
        let Some((canonical, kind)) = allowed.iter().find(|(n, _)| key == *n || key == &camel(n))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        let alias = camel(canonical);
        if key == &alias && alias != *canonical && a.contains_key(*canonical) {
            return Err(format!("Unexpected argument: {key}"));
        }
        let valid = match kind {
            Kind::String => value.is_string(),
            Kind::NullableString => value.is_null() || value.is_string(),
            Kind::Integer => value.as_i64().is_some(),
            Kind::NullableInteger => value.is_null() || value.as_i64().is_some(),
            Kind::Boolean => value.is_boolean(),
            Kind::NullableArray => value.is_null() || value.is_array(),
            Kind::ObjectArray => value.as_array().is_some_and(|v| {
                v.iter().all(|x| {
                    x.as_object()
                        .is_some_and(|o| o.values().all(Value::is_string))
                })
            }),
        };
        if !valid {
            let expected = match kind {
                Kind::String => "a string",
                Kind::NullableString => "a string or null",
                Kind::Integer => "an integer",
                Kind::NullableInteger => "an integer or null",
                Kind::Boolean => "a boolean",
                Kind::NullableArray => "an array or null",
                Kind::ObjectArray => "an array of objects with string values",
            };
            return Err(format!("{canonical} must be {expected}"));
        }
    }
    Ok(())
}
fn argument<'a>(a: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    a.get(name).or_else(|| a.get(&camel(name)))
}
fn required_string(a: &Map<String, Value>, name: &str) -> Result<String, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Err(format!("{name} is required")),
        Some(Value::String(v)) => Ok(v.clone()),
        Some(_) => Err(format!("{name} must be a string")),
    }
}
fn optional_string(a: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        Some(_) => Err(format!("{name} must be a string or null")),
    }
}
fn string(a: &Map<String, Value>, name: &str, default: &str) -> Result<String, String> {
    match argument(a, name) {
        None => Ok(default.into()),
        Some(Value::String(v)) => Ok(v.clone()),
        Some(_) => Err(format!("{name} must be a string")),
    }
}
fn required_integer(a: &Map<String, Value>, name: &str) -> Result<i64, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Err(format!("{name} is required")),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| format!("{name} must be an integer")),
    }
}
fn integer(a: &Map<String, Value>, name: &str, default: i64) -> Result<i64, String> {
    match argument(a, name) {
        None => Ok(default),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| format!("{name} must be an integer")),
    }
}
fn optional_integer(a: &Map<String, Value>, name: &str) -> Result<Option<i64>, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer or null")),
    }
}
fn boolean(a: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    match argument(a, name) {
        None => Ok(default),
        Some(Value::Bool(v)) => Ok(*v),
        Some(_) => Err(format!("{name} must be a boolean")),
    }
}
fn optional_array(a: &Map<String, Value>, name: &str) -> Result<Option<Vec<Value>>, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(v)) => Ok(Some(v.clone())),
        Some(_) => Err(format!("{name} must be an array or null")),
    }
}
fn object_array(a: &Map<String, Value>, name: &str) -> Result<Vec<Map<String, Value>>, String> {
    argument(a, name)
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_object).cloned().collect())
        .ok_or_else(|| format!("{name} is required"))
}
fn camel(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut upper = false;
    for c in v.chars() {
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
fn segment(v: &str) -> String {
    utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()
}
fn objects(v: &Value, key: &str) -> Vec<Map<String, Value>> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_object).cloned().collect())
        .unwrap_or_default()
}
fn field<'a>(v: &'a Map<String, Value>, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
fn map_field(v: &Map<String, Value>, key: &str, default: &str) -> String {
    v.get(key).map(display).unwrap_or_else(|| default.into())
}
fn vfield(v: &Value, key: &str, default: &str) -> String {
    v.get(key).map(display).unwrap_or_else(|| default.into())
}
fn display(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::String(v) => v.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Array(_) | Value::Object(_) => python_repr(v),
        _ => v.to_string(),
    }
}
fn python_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => python_string_repr(v),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_repr(&Value::String(key.clone())),
                    python_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn python_string_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut output = String::with_capacity(value.len() + 2);
    output.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\x08' => output.push_str("\\b"),
            '\x0c' => output.push_str("\\x0c"),
            value if value == quote => {
                output.push('\\');
                output.push(value);
            }
            value if value.is_control() => {
                let code = value as u32;
                if code <= 0xff {
                    output.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    output.push_str(&format!("\\u{code:04x}"));
                } else {
                    output.push_str(&format!("\\U{code:08x}"));
                }
            }
            value => output.push(value),
        }
    }
    output.push(quote);
    output
}
fn run_error(f: &str, e: &str) -> String {
    format!("Execution failed\nFunction: {f}\nError: {e}")
}
fn first_chars(v: &str, n: usize) -> (&str, bool) {
    v.char_indices()
        .nth(n)
        .map_or((v, false), |(i, _)| (&v[..i], true))
}
fn metric_lines(out: &mut Vec<String>, title: &str, values: &[Map<String, Value>], unit: &str) {
    if values.is_empty() {
        return;
    }
    out.push(title.into());
    for v in values {
        out.push(format!(
            "  {} to {}: {} {unit}",
            field(v, "startTime", "Unknown"),
            field(v, "endTime", "Unknown"),
            map_field(v, "value", "0")
        ));
    }
    out.push("".into());
}
