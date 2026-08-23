use std::collections::{HashSet, hash_map::DefaultHasher};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION};
use reqwest::{Method, StatusCode, Url};
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::auth::GoogleSession;

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3";
const READ: &str = "https://www.googleapis.com/auth/drive.readonly";
const WRITE: &str = "https://www.googleapis.com/auth/drive.file";
const FOLDER: &str = "application/vnd.google-apps.folder";
const SHORTCUT: &str = "application/vnd.google-apps.shortcut";
const DOC: &str = "application/vnd.google-apps.document";
const SHEET: &str = "application/vnd.google-apps.spreadsheet";
const SLIDE: &str = "application/vnd.google-apps.presentation";
const MAX_STREAM: u64 = 2 * 1024 * 1024 * 1024;
const MAX_CONTENT: u64 = 50 * 1024 * 1024;
const MAX_B64: usize = 32 * 1024 * 1024;
const UPLOAD_CHUNK: usize = 5 * 1024 * 1024;
static SEQ: AtomicU64 = AtomicU64::new(0);

const SEARCH: &[&str] = &[
    "user_google_email",
    "query",
    "page_size",
    "page_token",
    "drive_id",
    "include_items_from_all_drives",
    "corpora",
    "file_type",
    "detailed",
    "order_by",
    "include_trashed",
];
const ID: &[&str] = &["user_google_email", "file_id"];
const DOWNLOAD: &[&str] = &["user_google_email", "file_id", "export_format"];
const LIST: &[&str] = &[
    "user_google_email",
    "folder_id",
    "page_size",
    "page_token",
    "drive_id",
    "include_items_from_all_drives",
    "corpora",
    "file_type",
    "detailed",
    "order_by",
    "resource_type",
    "query",
    "include_organizers",
];
const MKDIR: &[&str] = &["user_google_email", "folder_name", "parent_folder_id"];
const CREATE: &[&str] = &[
    "user_google_email",
    "file_name",
    "content",
    "folder_id",
    "mime_type",
    "fileUrl",
    "base64_content",
    "content_mime_type",
];
const IMPORT: &[&str] = &[
    "user_google_email",
    "file_name",
    "content",
    "file_path",
    "file_url",
    "source_format",
    "folder_id",
];
const IMPORT_SLIDES: &[&str] = &[
    "user_google_email",
    "file_name",
    "file_path",
    "file_url",
    "source_format",
    "folder_id",
];
const PUBLIC: &[&str] = &["user_google_email", "file_name", "drive_id"];
const UPDATE: &[&str] = &[
    "user_google_email",
    "file_id",
    "name",
    "description",
    "mime_type",
    "add_parents",
    "remove_parents",
    "starred",
    "trashed",
    "writers_can_share",
    "copy_requires_writer_permission",
    "properties",
    "content",
    "file_path",
    "file_url",
    "source_format",
    "mode",
];
const ACCESS: &[&str] = &[
    "user_google_email",
    "file_id",
    "action",
    "share_with",
    "role",
    "share_type",
    "permission_id",
    "recipients",
    "send_notification",
    "email_message",
    "expiration_time",
    "allow_file_discovery",
    "new_owner_email",
    "move_to_new_owners_root",
];
const COPY: &[&str] = &[
    "user_google_email",
    "file_id",
    "new_name",
    "parent_folder_id",
];
const SET: &[&str] = &[
    "user_google_email",
    "file_id",
    "link_sharing",
    "writers_can_share",
    "copy_requires_writer_permission",
];

pub async fn dispatch(name: &str, a: &Map<String, Value>) -> Result<String, String> {
    match name {
        "search_drive_files" => search(a).await,
        "get_drive_file_content" => content(a).await,
        "get_drive_file_download_url" => download(a).await,
        "list_drive_items" => list(a).await,
        "create_drive_folder" => create_folder(a).await,
        "create_drive_file" => create_file(a).await,
        "import_to_google_doc" => import(a, Kind::Doc).await,
        "import_to_google_slides" => import(a, Kind::Slides).await,
        "import_to_google_sheets" => import(a, Kind::Sheets).await,
        "get_drive_file_permissions" => permissions(a).await,
        "check_drive_file_public_access" => public(a).await,
        "update_drive_file" => update(a).await,
        "get_drive_shareable_link" => shareable(a).await,
        "manage_drive_access" => access(a).await,
        "copy_drive_file" => copy(a).await,
        "set_drive_file_permissions" => set_permissions(a).await,
        _ => Err(format!("Unsupported native Drive tool: {name}")),
    }
}

async fn search(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SEARCH)?;
    let original = req_string(a, "query")?;
    let mut q = if structured(&original) {
        original.clone()
    } else {
        format!("fullText contains '{}'", esc(&original))
    };
    if !boolean(a, "include_trashed", false)? && !trashed_clause(&q) {
        q = format!("({q}) and trashed=false");
    }
    if let Some(t) = string(a, "file_type")? {
        q = format!("({q}) and mimeType = '{}'", file_type(&t)?);
    }
    let detailed = boolean(a, "detailed", true)?;
    let s = auth(a, READ).await?;
    let r = files_list(
        &s,
        "search_drive_files",
        q,
        integer(a, "page_size", 10)?,
        FilesListOptions {
            page_token: string(a, "page_token")?,
            drive_id: string(a, "drive_id")?,
            include_items_from_all_drives: boolean(a, "include_items_from_all_drives", true)?,
            corpora: string(a, "corpora")?,
            order_by: string(a, "order_by")?,
            detailed,
            include_permissions: detailed,
        },
    )
    .await?;
    let files = arr(&r, "files");
    if files.is_empty() {
        return Ok(format!("No files found for '{original}'."));
    }
    let mut out = vec![format!(
        "Found {} files for {} matching '{original}':",
        files.len(),
        s.user_email
    )];
    out.extend(files.iter().map(|f| format_file(f, detailed, true)));
    next(&mut out, &r);
    Ok(out.join("\n"))
}

async fn content(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, ID)?;
    let s = auth(a, READ).await?;
    let (id, m) = resolve(
        &s,
        "get_drive_file_content",
        &req_string(a, "file_id")?,
        "name,webViewLink",
    )
    .await?;
    let mime = text(&m, "mimeType").unwrap_or("");
    let export = match mime {
        DOC | SLIDE => Some("text/plain"),
        SHEET => Some("text/csv"),
        _ => None,
    };
    let bytes = get_bytes(&s, "get_drive_file_content", &id, export, MAX_CONTENT).await?;
    let body = if mime.starts_with("image/") {
        format!("[base64_image:{mime}]{}", b64_encode(&bytes))
    } else {
        String::from_utf8(bytes).unwrap_or_else(|e| {
            format!(
                "[Binary or unsupported text encoding for mimeType '{mime}' - {} bytes]",
                e.as_bytes().len()
            )
        })
    };
    Ok(format!(
        "File: \"{}\" (ID: {id}, Type: {mime})\nLink: {}\n\n--- CONTENT ---\n{body}",
        text(&m, "name").unwrap_or("Unknown File"),
        text(&m, "webViewLink").unwrap_or("#")
    ))
}

async fn download(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, DOWNLOAD)?;
    let s = auth(a, READ).await?;
    let (id, m) = resolve(
        &s,
        "get_drive_file_download_url",
        &req_string(a, "file_id")?,
        "name,webViewLink",
    )
    .await?;
    let mime = text(&m, "mimeType").unwrap_or("application/octet-stream");
    let name = text(&m, "name").unwrap_or("Unknown File");
    let (export, out_mime, out_name) = export_choice(
        mime,
        name,
        &string(a, "export_format")?
            .unwrap_or_default()
            .to_lowercase(),
    );
    let tmp = Temp::new("drive_download")?;
    let size = get_file(
        &s,
        "get_drive_file_download_url",
        &id,
        export,
        &tmp.path,
        MAX_STREAM,
    )
    .await?;
    let kb = size as f64 / 1024.0;
    if stateless() {
        let mut f = File::open(&tmp.path).map_err(|e| e.to_string())?;
        let mut p = vec![0; 100.min(size as usize)];
        f.read_exact(&mut p).map_err(|e| e.to_string())?;
        return Ok(format!(
            "File downloaded successfully!\nFile: {name}\nFile ID: {id}\nSize: {kb:.1} KB ({size} bytes)\nMIME Type: {out_mime}\n\nStateless mode: File storage disabled.\n\nBase64-encoded content (first 100 characters shown):\n{}...",
            b64_encode(&p)
        ));
    }
    let saved = save_attachment(&tmp.path, &out_name, &s.user_email)?;
    tmp.disarm();
    let mut out = vec![
        "File downloaded successfully!".into(),
        format!("File: {name}"),
        format!("File ID: {id}"),
        format!("Size: {kb:.1} KB ({size} bytes)"),
        format!("MIME Type: {out_mime}"),
    ];
    if transport() == "stdio" {
        out.push(format!("\nSaved to: {}", saved.path.display()));
        out.push(
            "\nThe file has been saved to disk and can be accessed directly via the file path."
                .into(),
        )
    } else {
        out.push(format!("\nDownload URL: {}", attachment_url(&saved.id)?));
        out.push("\nThe file will expire after 1 hour.".into())
    }
    if export.is_some() {
        out.push(format!(
            "\nNote: Google native file exported to {out_mime} format."
        ))
    }
    Ok(out.join("\n"))
}

async fn list(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, LIST)?;
    let s = auth(a, READ).await?;
    let kind = or_string(a, "resource_type", "items")?
        .trim()
        .to_lowercase();
    if kind == "shared_drives" {
        return shared_drives(a, &s).await;
    }
    if kind != "items" {
        return Err("resource_type must be either 'items' or 'shared_drives'".into());
    }
    let input = or_string(a, "folder_id", "root")?;
    let folder = resolve_folder(&s, "list_drive_items", &input).await?;
    let mut q = format!("'{}' in parents and trashed=false", esc(&folder));
    if let Some(t) = string(a, "file_type")? {
        q = format!("({q}) and mimeType = '{}'", file_type(&t)?)
    }
    let detailed = boolean(a, "detailed", true)?;
    let r = files_list(
        &s,
        "list_drive_items",
        q,
        integer(a, "page_size", 100)?,
        FilesListOptions {
            page_token: string(a, "page_token")?,
            drive_id: string(a, "drive_id")?,
            include_items_from_all_drives: boolean(a, "include_items_from_all_drives", true)?,
            corpora: string(a, "corpora")?,
            order_by: string(a, "order_by")?,
            detailed,
            include_permissions: false,
        },
    )
    .await?;
    let items = arr(&r, "files");
    if items.is_empty() {
        return Ok(format!("No items found in folder '{input}'."));
    }
    let mut out = vec![format!(
        "Found {} items in folder '{input}' for {}:",
        items.len(),
        s.user_email
    )];
    out.extend(items.iter().map(|f| format_file(f, detailed, false)));
    next(&mut out, &r);
    Ok(out.join("\n"))
}

async fn shared_drives(a: &Map<String, Value>, s: &GoogleSession) -> Result<String, String> {
    let mut q=vec![("pageSize".into(),integer(a,"page_size",100)?.clamp(1,100).to_string()),("fields".into(),"drives(id,name,createdTime,hidden,restrictions,capabilities(canManageMembers,canEdit)),nextPageToken".into())];
    push(&mut q, "pageToken", string(a, "page_token")?);
    push(&mut q, "q", string(a, "query")?);
    let r = send(
        s,
        "list_drive_items",
        Method::GET,
        &format!("{API}/drives"),
        &q,
        None,
    )
    .await?;
    let drives = arr(&r, "drives");
    if drives.is_empty() {
        return Ok(format!("No shared drives found for {}.", s.user_email));
    }
    let org = boolean(a, "include_organizers", false)?;
    let mut out = vec![format!(
        "Found {} shared drives for {}:",
        drives.len(),
        s.user_email
    )];
    for d in drives {
        out.push(format!(
            "- Name: \"{}\" (ID: {}, Created: {}){} Capabilities: {}; Restrictions: {}",
            text(d, "name").unwrap_or(""),
            text(d, "id").unwrap_or(""),
            text(d, "createdTime").unwrap_or("N/A"),
            if flag(d, "hidden").unwrap_or(false) {
                " [hidden]"
            } else {
                ""
            },
            true_keys(d.get("capabilities")),
            true_keys(d.get("restrictions"))
        ));
        if org {
            match all_permissions(s, "list_drive_items", text(d, "id").unwrap_or("")).await {
                Ok(p) => {
                    let p = p
                        .iter()
                        .filter(|v| text(v, "role") == Some("organizer"))
                        .collect::<Vec<_>>();
                    if p.is_empty() {
                        out.push("  Organizers: <none returned>".into())
                    }
                    for x in p {
                        out.push(format!(
                            "  Organizer ({}): {}",
                            text(x, "type").unwrap_or("?"),
                            text(x, "emailAddress")
                                .or_else(|| text(x, "domain"))
                                .or_else(|| text(x, "displayName"))
                                .unwrap_or("?")
                        ))
                    }
                }
                Err(e) => out.push(format!("  Organizers: <error: {e}>")),
            }
        }
    }
    next(&mut out, &r);
    Ok(out.join("\n"))
}

async fn create_folder(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, MKDIR)?;
    let s = auth(a, WRITE).await?;
    folder_impl(
        &s,
        &req_string(a, "folder_name")?,
        &or_string(a, "parent_folder_id", "root")?,
    )
    .await
}
async fn folder_impl(s: &GoogleSession, name: &str, parent_input: &str) -> Result<String, String> {
    let parent = resolve_folder(s, "create_drive_folder", parent_input).await?;
    let r = send(
        s,
        "create_drive_folder",
        Method::POST,
        &format!("{API}/files"),
        &[
            ("supportsAllDrives".into(), "true".into()),
            ("fields".into(), "id,name,webViewLink".into()),
        ],
        Some(&json!({"name":name,"parents":[parent],"mimeType":FOLDER})),
    )
    .await?;
    Ok(format!(
        "Successfully created folder '{}' (ID: {}) in folder '{parent_input}' for {}. Link: {}",
        text(&r, "name").unwrap_or(name),
        text(&r, "id").unwrap_or("N/A"),
        s.user_email,
        text(&r, "webViewLink").unwrap_or("")
    ))
}

async fn create_file(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, CREATE)?;
    let name = req_string(a, "file_name")?;
    let s = auth(a, WRITE).await?;
    let folder_input = or_string(a, "folder_id", "root")?;
    let mut mime = or_string(a, "mime_type", "text/plain")?;
    valid_mime(&mime)?;
    let content = string(a, "content")?;
    let url = string(a, "fileUrl")?;
    let encoded = string(a, "base64_content")?;
    let cmime = string(a, "content_mime_type")?;
    if mime == FOLDER {
        return folder_impl(&s, &name, &folder_input).await;
    }
    if content.is_none() && url.as_deref().is_none_or(str::is_empty) && encoded.is_none() {
        return Err("You must provide one of 'content', 'fileUrl', or 'base64_content'.".into());
    }
    if encoded.is_some() && (content.is_some() || url.as_deref().is_some_and(|x| !x.is_empty())) {
        return Err("'base64_content' cannot be used with 'content' or 'fileUrl'.".into());
    }
    if cmime.is_some() && encoded.is_none() {
        return Err("'content_mime_type' can only be used with 'base64_content'.".into());
    }
    if encoded.is_some() && cmime.as_deref().is_none_or(str::is_empty) {
        return Err("'content_mime_type' is required when using 'base64_content'.".into());
    }
    let mut p = if let Some(v) = encoded {
        if v.len() > max_b64(MAX_B64) {
            return Err(format!("'base64_content' exceeds the {MAX_B64} byte limit"));
        }
        let b = b64_decode(&v)?;
        if b.len() > MAX_B64 {
            return Err(format!("'base64_content' exceeds the {MAX_B64} byte limit"));
        }
        mime = cmime.unwrap();
        valid_mime(&mime)?;
        Payload::bytes(b, &mime)
    } else if let Some(v) = url.filter(|x| !x.is_empty()) {
        let x = url_payload(&v, &mime, true).await?;
        if let Some(m) = &x.remote_mime {
            mime = m.clone()
        }
        x
    } else {
        Payload::bytes(content.unwrap_or_default().into_bytes(), &mime)
    };
    p.mime = mime.clone();
    let parent = resolve_folder(&s, "create_drive_file", &folder_input).await?;
    let r = resumable(
        &s,
        "create_drive_file",
        Method::POST,
        None,
        &[],
        &json!({"name":name,"parents":[parent],"mimeType":mime}),
        &mut p,
    )
    .await?;
    Ok(format!(
        "Successfully created file '{}' (ID: {}) in folder '{folder_input}' for {}. Link: {}",
        text(&r, "name").unwrap_or(&name),
        text(&r, "id").unwrap_or("N/A"),
        s.user_email,
        text(&r, "webViewLink").unwrap_or("No link available")
    ))
}

#[derive(Clone, Copy)]
enum Kind {
    Doc,
    Slides,
    Sheets,
}
const DOC_FORMATS: &[(&str, &str)] = &[
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("html", "text/html"),
    ("htm", "text/html"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("doc", "application/msword"),
    ("rtf", "application/rtf"),
    ("odt", "application/vnd.oasis.opendocument.text"),
];
const SLIDE_FORMATS: &[(&str, &str)] = &[
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("ppt", "application/vnd.ms-powerpoint"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
];
const SHEET_FORMATS: &[(&str, &str)] = &[
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xls", "application/vnd.ms-excel"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
];
impl Kind {
    fn tool(self) -> &'static str {
        match self {
            Self::Doc => "import_to_google_doc",
            Self::Slides => "import_to_google_slides",
            Self::Sheets => "import_to_google_sheets",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Self::Doc => DOC,
            Self::Slides => SLIDE,
            Self::Sheets => SHEET,
        }
    }
    fn formats(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Doc => DOC_FORMATS,
            Self::Slides => SLIDE_FORMATS,
            Self::Sheets => SHEET_FORMATS,
        }
    }
    fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Doc => ("Google Doc", "Document ID"),
            Self::Slides => ("Google Slides presentation", "Presentation ID"),
            Self::Sheets => ("Google Sheets spreadsheet", "Spreadsheet ID"),
        }
    }
}
async fn import(a: &Map<String, Value>, k: Kind) -> Result<String, String> {
    validate(
        a,
        if matches!(k, Kind::Slides) {
            IMPORT_SLIDES
        } else {
            IMPORT
        },
    )?;
    let name = req_string(a, "file_name")?;
    let s = auth(a, WRITE).await?;
    let folder_input = or_string(a, "folder_id", "root")?;
    let mut p = import_payload(a, k.tool(), &name, k.formats(), None).await?;
    let source = p.mime.clone();
    let parent = resolve_folder(&s, k.tool(), &folder_input).await?;
    let clean = stem(&name);
    let r = resumable(
        &s,
        k.tool(),
        Method::POST,
        None,
        &[],
        &json!({"name":clean,"parents":[parent],"mimeType":k.mime()}),
        &mut p,
    )
    .await?;
    let (label, idlabel) = k.labels();
    Ok(format!(
        "Successfully imported '{clean}' as {label}\n   {idlabel}: {}\n   Source format: {source}\n   Folder: {folder_input}\n   Link: {}",
        text(&r, "id").unwrap_or("N/A"),
        text(&r, "webViewLink").unwrap_or("No link available")
    ))
}

async fn permissions(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, ID)?;
    let s = auth(a, READ).await?;
    match permissions_impl(&s, &req_string(a, "file_id")?).await {
        Ok(x) => Ok(x),
        Err(e) => Ok(format!("Error getting file permissions: {e}")),
    }
}
async fn permissions_impl(s: &GoogleSession, input: &str) -> Result<String, String> {
    let (id, _) = resolve(s, "get_drive_file_permissions", input, "").await?;
    let m=send(s,"get_drive_file_permissions",Method::GET,&file_url(&id),&[("supportsAllDrives".into(),"true".into()),("fields".into(),"id,name,mimeType,size,parents,createdTime,modifiedTime,trashed,driveId,owners(displayName,emailAddress),permissions(id,type,role,emailAddress,domain,expirationTime,permissionDetails),webViewLink,webContentLink,shared,sharingUser".into())],None).await?;
    let p = if m.get("driveId").is_some() {
        all_permissions(s, "get_drive_file_permissions", &id).await?
    } else {
        arr(&m, "permissions").to_vec()
    };
    let parents = arr(&m, "parents")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let owners = arr(&m, "owners")
        .iter()
        .map(|o| {
            format!(
                "{}{}",
                text(o, "displayName").unwrap_or("Unknown"),
                text(o, "emailAddress")
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = vec![
        format!("File: {}", text(&m, "name").unwrap_or("Unknown")),
        format!("ID: {id}"),
        format!("Type: {}", text(&m, "mimeType").unwrap_or("Unknown")),
        format!(
            "Parents: {}",
            if parents.is_empty() {
                "None (root or orphaned)"
            } else {
                &parents
            }
        ),
        format!(
            "Owners: {}",
            if owners.is_empty() {
                "None available"
            } else {
                &owners
            }
        ),
        format!("Size: {} bytes", value(m.get("size"), "N/A")),
        format!("Created: {}", text(&m, "createdTime").unwrap_or("N/A")),
        format!("Modified: {}", text(&m, "modifiedTime").unwrap_or("N/A")),
        format!("Trashed: {}", flag(&m, "trashed").unwrap_or(false)),
    ];
    if let Some(d) = text(&m, "driveId") {
        out.push(format!("Shared Drive ID: {d}"))
    }
    out.extend([
        "".into(),
        "Sharing Status:".into(),
        format!("  Shared: {}", is_shared(&m, &p)),
    ]);
    if p.is_empty() {
        out.push("  No additional permissions (private file)".into())
    } else {
        out.push(format!("  Number of permissions: {}", p.len()));
        out.push("  Permissions:".into());
        out.extend(p.iter().map(|x| format!("    - {}", perm(x))))
    }
    out.extend([
        "".into(),
        "URLs:".into(),
        format!("  View Link: {}", text(&m, "webViewLink").unwrap_or("N/A")),
    ]);
    if let Some(x) = text(&m, "webContentLink") {
        out.push(format!("  Direct Download Link: {x}"))
    }
    out.push("".into());
    out.push(if is_public(&p){"This file is shared with 'Anyone with the link' - it can be inserted into Google Docs"}else{"This file is NOT shared with 'Anyone with the link' - it cannot be inserted into Google Docs"}.into());
    Ok(out.join("\n"))
}

async fn public(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, PUBLIC)?;
    let name = req_string(a, "file_name")?;
    let s = auth(a, READ).await?;
    let mut q = vec![
        ("q".into(), format!("name = '{}'", esc(&name))),
        ("pageSize".into(), "10".into()),
        (
            "fields".into(),
            "files(id,name,mimeType,webViewLink)".into(),
        ),
        ("supportsAllDrives".into(), "true".into()),
        ("includeItemsFromAllDrives".into(), "true".into()),
    ];
    if let Some(d) = string(a, "drive_id")? {
        valid_id(&d, "drive_id")?;
        q.extend([("corpora".into(), "drive".into()), ("driveId".into(), d)])
    }
    let r = send(
        &s,
        "check_drive_file_public_access",
        Method::GET,
        &format!("{API}/files"),
        &q,
        None,
    )
    .await?;
    let files = arr(&r, "files");
    if files.is_empty() {
        return Ok(format!("No file found with name '{name}'"));
    }
    let mut out = Vec::new();
    if files.len() > 1 {
        out.push(format!("Found {} files with name '{name}':", files.len()));
        out.extend(files.iter().map(|f| {
            format!(
                "  - {} (ID: {})",
                text(f, "name").unwrap_or(""),
                text(f, "id").unwrap_or("")
            )
        }));
        out.push("\nChecking the first file...\n".into())
    }
    let (id, _) = resolve(
        &s,
        "check_drive_file_public_access",
        text(&files[0], "id").unwrap_or(""),
        "",
    )
    .await?;
    let m = send(
        &s,
        "check_drive_file_public_access",
        Method::GET,
        &file_url(&id),
        &[
            ("supportsAllDrives".into(), "true".into()),
            (
                "fields".into(),
                "id,name,mimeType,driveId,permissions,webViewLink,webContentLink,shared".into(),
            ),
        ],
        None,
    )
    .await?;
    let p = if m.get("driveId").is_some() {
        all_permissions(&s, "check_drive_file_public_access", &id).await?
    } else {
        arr(&m, "permissions").to_vec()
    };
    out.extend([
        format!("File: {}", text(&m, "name").unwrap_or("Unknown")),
        format!("ID: {id}"),
        format!("Type: {}", text(&m, "mimeType").unwrap_or("Unknown")),
        format!("Shared: {}", is_shared(&m, &p)),
        "".into(),
    ]);
    if is_public(&p) {
        out.push("PUBLIC ACCESS ENABLED - This file can be inserted into Google Docs".into());
        out.push(format!(
            "Use with insert_doc_image_url: https://drive.google.com/uc?export=view&id={}",
            segment(&id)
        ))
    } else {
        out.push("NO PUBLIC ACCESS - Cannot insert into Google Docs".into());
        out.push("Fix: Drive -> Share -> 'Anyone with the link' -> 'Viewer'".into())
    }
    Ok(out.join("\n"))
}

async fn update(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, UPDATE)?;
    let mode = or_string(a, "mode", "replace")?;
    if !["replace", "append", "prepend"].contains(&mode.as_str()) {
        return Err(format!(
            "Unsupported mode: '{mode}'. Supported: replace, append, prepend."
        ));
    }
    if mode != "replace" && string(a, "mime_type")?.is_some() {
        return Err(format!("mime_type cannot be set when mode='{mode}'."));
    }
    if mode != "replace" && string(a, "content")?.is_none() {
        return Err(format!("mode='{mode}' requires 'content'."));
    }
    let s = auth(a, WRITE).await?;
    let(id,current)=resolve(&s,"update_drive_file",&req_string(a,"file_id")?,"name,description,parents,starred,trashed,webViewLink,writersCanShare,copyRequiresWriterPermission,properties").await?;
    let mut body = Map::new();
    for (arg, key) in [
        ("name", "name"),
        ("description", "description"),
        ("mime_type", "mimeType"),
    ] {
        if let Some(v) = string(a, arg)? {
            body.insert(key.into(), Value::String(v));
        }
    }
    for (arg, key) in [
        ("starred", "starred"),
        ("trashed", "trashed"),
        ("writers_can_share", "writersCanShare"),
        (
            "copy_requires_writer_permission",
            "copyRequiresWriterPermission",
        ),
    ] {
        if let Some(v) = optional_bool(a, arg)? {
            body.insert(key.into(), Value::Bool(v));
        }
    }
    if let Some(v) = object(a, "properties")? {
        body.insert("properties".into(), Value::Object(v));
    }
    let mut q=vec![("supportsAllDrives".into(),"true".into()),("fields".into(),"id,name,description,mimeType,parents,starred,trashed,webViewLink,writersCanShare,copyRequiresWriterPermission,properties".into())];
    if let Some(v) = parents(&s, "update_drive_file", string(a, "add_parents")?).await? {
        q.push(("addParents".into(), v))
    }
    if let Some(v) = parents(&s, "update_drive_file", string(a, "remove_parents")?).await? {
        q.push(("removeParents".into(), v))
    }
    let count = [
        string(a, "content")?.is_some(),
        string(a, "file_path")?.is_some(),
        string(a, "file_url")?.is_some(),
    ]
    .into_iter()
    .filter(|v| *v)
    .count();
    if count > 1 {
        return Err("Provide only one of: 'content', 'file_path', or 'file_url'.".into());
    }
    let replacing = count == 1;
    let target =
        string(a, "mime_type")?.unwrap_or_else(|| text(&current, "mimeType").unwrap_or("").into());
    let r = if replacing {
        let _lock = Lock::take(&id)?;
        let mut synthetic = a.clone();
        if mode != "replace" {
            if target.starts_with("application/vnd.google-apps.") {
                return Err(format!(
                    "mode='{mode}' is only supported for non-Google files"
                ));
            }
            let old = String::from_utf8(
                get_bytes(&s, "update_drive_file", &id, None, MAX_CONTENT).await?,
            )
            .map_err(|_| format!("mode='{mode}' requires a UTF-8 text file"))?;
            let added = string(a, "content")?.unwrap_or_default();
            synthetic.insert("content".into(), Value::String(splice(&old, &added, &mode)));
        }
        let formats = match target.as_str() {
            DOC => Some(DOC_FORMATS),
            SHEET => Some(SHEET_FORMATS),
            SLIDE => Some(SLIDE_FORMATS),
            x if x.starts_with("application/vnd.google-apps.") => {
                return Err(format!(
                    "Content replacement is not supported for this Google Apps type ({x})."
                ));
            }
            _ => None,
        };
        let name =
            string(a, "name")?.unwrap_or_else(|| text(&current, "name").unwrap_or("").into());
        let mut p = import_payload(
            &synthetic,
            "update_drive_file",
            &name,
            formats.unwrap_or(&[]),
            formats.is_none().then_some(target.as_str()),
        )
        .await?;
        resumable(
            &s,
            "update_drive_file",
            Method::PATCH,
            Some(&id),
            &q,
            &Value::Object(body),
            &mut p,
        )
        .await?
    } else {
        send(
            &s,
            "update_drive_file",
            Method::PATCH,
            &file_url(&id),
            &q,
            Some(&Value::Object(body)),
        )
        .await?
    };
    let mut out = vec![
        format!(
            "Successfully updated file: {}",
            text(&r, "name")
                .or_else(|| text(&current, "name"))
                .unwrap_or("Unknown")
        ),
        format!("   File ID: {id}"),
    ];
    out.push(if replacing {
        format!("   - {mode} content")
    } else {
        "   (Metadata update completed)".into()
    });
    out.push(format!(
        "\nView file: {}",
        text(&r, "webViewLink")
            .or_else(|| text(&current, "webViewLink"))
            .unwrap_or("#")
    ));
    Ok(out.join("\n"))
}

async fn shareable(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, ID)?;
    let s = auth(a, READ).await?;
    let (id, _) = resolve(
        &s,
        "get_drive_shareable_link",
        &req_string(a, "file_id")?,
        "",
    )
    .await?;
    let m=send(&s,"get_drive_shareable_link",Method::GET,&file_url(&id),&[("supportsAllDrives".into(),"true".into()),("fields".into(),"id,name,mimeType,webViewLink,webContentLink,shared,permissions(id,type,role,emailAddress,domain,expirationTime)".into())],None).await?;
    let mut out = vec![
        format!("File: {}", text(&m, "name").unwrap_or("Unknown")),
        format!("ID: {id}"),
        format!("Type: {}", text(&m, "mimeType").unwrap_or("Unknown")),
        format!("Shared: {}", flag(&m, "shared").unwrap_or(false)),
        "".into(),
        "Links:".into(),
        format!("  View: {}", text(&m, "webViewLink").unwrap_or("N/A")),
    ];
    if let Some(x) = text(&m, "webContentLink") {
        out.push(format!("  Download: {x}"))
    }
    if !arr(&m, "permissions").is_empty() {
        out.push("\nCurrent permissions:".into());
        out.extend(
            arr(&m, "permissions")
                .iter()
                .map(|p| format!("  - {}", perm(p))),
        )
    }
    Ok(out.join("\n"))
}

async fn access(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, ACCESS)?;
    let action = req_string(a, "action")?;
    if !["grant", "grant_batch", "update", "revoke", "transfer_owner"].contains(&action.as_str()) {
        return Err(format!(
            "Invalid action '{action}'. Must be one of: grant, grant_batch, update, revoke, transfer_owner"
        ));
    }
    let s = auth(a, WRITE).await?;
    let (id, m) = resolve(
        &s,
        "manage_drive_access",
        &req_string(a, "file_id")?,
        "name,webViewLink,owners",
    )
    .await?;
    match action.as_str() {
        "grant" => grant(a, &s, &id, &m).await,
        "grant_batch" => grant_batch(a, &s, &id, &m).await,
        "update" => update_permission(a, &s, &id, &m).await,
        "revoke" => {
            let pid = string(a, "permission_id")?
                .filter(|v| !v.is_empty())
                .ok_or("permission_id is required for 'revoke' action")?;
            send(
                &s,
                "manage_drive_access",
                Method::DELETE,
                &permission_url(&id, &pid),
                &[("supportsAllDrives".into(), "true".into())],
                None,
            )
            .await?;
            Ok(format!(
                "Successfully removed permission from '{}'\n\nPermission ID '{pid}' has been revoked.",
                text(&m, "name").unwrap_or("Unknown")
            ))
        }
        _ => {
            let owner = string(a, "new_owner_email")?
                .filter(|v| !v.is_empty())
                .ok_or("new_owner_email is required for 'transfer_owner' action")?;
            send(
                &s,
                "manage_drive_access",
                Method::POST,
                &permissions_url(&id),
                &[
                    ("transferOwnership".into(), "true".into()),
                    (
                        "moveToNewOwnersRoot".into(),
                        boolean(a, "move_to_new_owners_root", false)?.to_string(),
                    ),
                    ("supportsAllDrives".into(), "true".into()),
                    ("fields".into(), "id,type,role,emailAddress".into()),
                ],
                Some(&json!({"type":"user","role":"owner","emailAddress":owner})),
            )
            .await?;
            Ok(format!(
                "Successfully transferred ownership of '{}'\n\nNew owner: {owner}\n\nNote: Previous owner now has editor access.",
                text(&m, "name").unwrap_or("Unknown")
            ))
        }
    }
}
async fn grant(
    a: &Map<String, Value>,
    s: &GoogleSession,
    id: &str,
    m: &Value,
) -> Result<String, String> {
    let role = string(a, "role")?.unwrap_or("reader".into());
    let kind = or_string(a, "share_type", "user")?;
    let who = string(a, "share_with")?;
    let body = perm_body(
        &kind,
        &role,
        who.as_deref(),
        string(a, "expiration_time")?.as_deref(),
        optional_bool(a, "allow_file_discovery")?,
    )?;
    let mut q = vec![
        ("supportsAllDrives".into(), "true".into()),
        (
            "fields".into(),
            "id,type,role,emailAddress,domain,expirationTime".into(),
        ),
    ];
    if ["user", "group"].contains(&kind.as_str()) {
        q.push((
            "sendNotificationEmail".into(),
            boolean(a, "send_notification", true)?.to_string(),
        ));
        push(&mut q, "emailMessage", string(a, "email_message")?)
    }
    let p = send(
        s,
        "manage_drive_access",
        Method::POST,
        &permissions_url(id),
        &q,
        Some(&body),
    )
    .await?;
    Ok(format!(
        "Successfully shared '{}'\n\nPermission created:\n  - {}\n\nView link: {}",
        text(m, "name").unwrap_or("Unknown"),
        perm(&p),
        text(m, "webViewLink").unwrap_or("N/A")
    ))
}
async fn grant_batch(
    a: &Map<String, Value>,
    s: &GoogleSession,
    id: &str,
    m: &Value,
) -> Result<String, String> {
    let recipients = array_arg(a, "recipients")?
        .filter(|v| !v.is_empty())
        .ok_or("recipients list is required for 'grant_batch' action")?;
    let (mut ok, mut bad) = (0, 0);
    let mut out = Vec::new();
    for r in recipients {
        let Some(o) = r.as_object() else {
            bad += 1;
            out.push("  - Failed: recipient must be an object".into());
            continue;
        };
        let kind = o
            .get("share_type")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let who = if kind == "domain" {
            o.get("domain")
        } else {
            o.get("email")
        }
        .and_then(Value::as_str);
        let role = o.get("role").and_then(Value::as_str).unwrap_or("reader");
        let exp = o.get("expiration_time").and_then(Value::as_str);
        match perm_body(kind, role, who, exp, None) {
            Ok(body) => {
                let mut params = vec![
                    ("supportsAllDrives".into(), "true".into()),
                    (
                        "fields".into(),
                        "id,type,role,emailAddress,domain,expirationTime".into(),
                    ),
                ];
                if ["user", "group"].contains(&kind) {
                    params.push((
                        "sendNotificationEmail".into(),
                        boolean(a, "send_notification", true)?.to_string(),
                    ));
                    push(&mut params, "emailMessage", string(a, "email_message")?);
                }
                match send(
                    s,
                    "manage_drive_access",
                    Method::POST,
                    &permissions_url(id),
                    &params,
                    Some(&body),
                )
                .await
                {
                    Ok(p) => {
                        ok += 1;
                        out.push(format!("  - {}", perm(&p)))
                    }
                    Err(e) => {
                        bad += 1;
                        out.push(format!("  - {}: Failed - {e}", who.unwrap_or("<missing>")))
                    }
                }
            }
            Err(e) => {
                bad += 1;
                out.push(format!("  - {}: Failed - {e}", who.unwrap_or("<missing>")))
            }
        }
    }
    Ok(format!(
        "Batch share results for '{}'\n\nSummary: {ok} succeeded, {bad} failed\n\nResults:\n{}\n\nView link: {}",
        text(m, "name").unwrap_or("Unknown"),
        out.join("\n"),
        text(m, "webViewLink").unwrap_or("N/A")
    ))
}
async fn update_permission(
    a: &Map<String, Value>,
    s: &GoogleSession,
    id: &str,
    m: &Value,
) -> Result<String, String> {
    let pid = string(a, "permission_id")?
        .filter(|v| !v.is_empty())
        .ok_or("permission_id is required for 'update' action")?;
    let role = string(a, "role")?;
    let exp = string(a, "expiration_time")?;
    if role.is_none() && exp.is_none() {
        return Err(
            "Must provide at least one of: role, expiration_time for 'update' action".into(),
        );
    }
    if let Some(r) = &role {
        valid_role(r)?
    }
    if let Some(e) = &exp {
        valid_exp(e)?
    }
    let role = if let Some(r) = role {
        r
    } else {
        let p = send(
            s,
            "manage_drive_access",
            Method::GET,
            &permission_url(id, &pid),
            &[
                ("supportsAllDrives".into(), "true".into()),
                ("fields".into(), "role".into()),
            ],
            None,
        )
        .await?;
        text(&p, "role").unwrap_or("reader").into()
    };
    let mut body = json!({"role":role});
    if let Some(e) = exp {
        body["expirationTime"] = Value::String(e)
    }
    let p = send(
        s,
        "manage_drive_access",
        Method::PATCH,
        &permission_url(id, &pid),
        &[
            ("supportsAllDrives".into(), "true".into()),
            (
                "fields".into(),
                "id,type,role,emailAddress,domain,expirationTime".into(),
            ),
        ],
        Some(&body),
    )
    .await?;
    Ok(format!(
        "Successfully updated permission on '{}'\n\nUpdated permission:\n  - {}",
        text(m, "name").unwrap_or("Unknown"),
        perm(&p)
    ))
}

async fn copy(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, COPY)?;
    let s = auth(a, WRITE).await?;
    let (id, m) = resolve(
        &s,
        "copy_drive_file",
        &req_string(a, "file_id")?,
        "name,webViewLink",
    )
    .await?;
    let folder_input = or_string(a, "parent_folder_id", "root")?;
    let parent = resolve_folder(&s, "copy_drive_file", &folder_input).await?;
    let original = text(&m, "name").unwrap_or("Unknown File");
    let name = string(a, "new_name")?.unwrap_or_else(|| format!("Copy of {original}"));
    let mut body = json!({"name":name});
    if parent != "root" {
        body["parents"] = json!([parent])
    }
    let r = send(
        &s,
        "copy_drive_file",
        Method::POST,
        &format!("{}/copy", file_url(&id)),
        &[
            ("supportsAllDrives".into(), "true".into()),
            (
                "fields".into(),
                "id,name,webViewLink,mimeType,parents".into(),
            ),
        ],
        Some(&body),
    )
    .await?;
    Ok(format!(
        "Successfully copied '{original}'\n\nOriginal file ID: {id}\nNew file ID: {}\nNew file name: {}\nFile type: {}\nLocation: {folder_input}\n\nView copied file: {}",
        text(&r, "id").unwrap_or("N/A"),
        text(&r, "name").unwrap_or("Unknown"),
        text(&r, "mimeType").unwrap_or("Unknown"),
        text(&r, "webViewLink").unwrap_or("N/A")
    ))
}

async fn set_permissions(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, SET)?;
    let link = string(a, "link_sharing")?;
    let writers = optional_bool(a, "writers_can_share")?;
    let copy = optional_bool(a, "copy_requires_writer_permission")?;
    if link.is_none() && writers.is_none() && copy.is_none() {
        return Err("Must provide at least one of: link_sharing, writers_can_share, copy_requires_writer_permission".into());
    }
    if let Some(v) = &link
        && !["off", "reader", "commenter", "writer"].contains(&v.as_str())
    {
        return Err(format!(
            "Invalid link_sharing '{v}'. Must be one of: commenter, off, reader, writer"
        ));
    }
    let s = auth(a, WRITE).await?;
    let (id, m) = resolve(
        &s,
        "set_drive_file_permissions",
        &req_string(a, "file_id")?,
        "name,webViewLink",
    )
    .await?;
    let mut changes = Vec::new();
    let mut body = Map::new();
    if let Some(v) = writers {
        body.insert("writersCanShare".into(), Value::Bool(v));
        changes.push(format!(
            "  - Editors sharing: {}",
            if v { "allowed" } else { "restricted to owner" }
        ))
    }
    if let Some(v) = copy {
        body.insert("copyRequiresWriterPermission".into(), Value::Bool(v));
        changes.push(format!(
            "  - Viewers copy/print/download: {}",
            if v { "restricted" } else { "allowed" }
        ))
    }
    if !body.is_empty() {
        send(
            &s,
            "set_drive_file_permissions",
            Method::PATCH,
            &file_url(&id),
            &[
                ("supportsAllDrives".into(), "true".into()),
                ("fields".into(), "id".into()),
            ],
            Some(&Value::Object(body)),
        )
        .await?;
    }
    if let Some(v) = link {
        let p = all_permissions(&s, "set_drive_file_permissions", &id).await?;
        let anyone = p
            .iter()
            .filter(|x| text(x, "type") == Some("anyone"))
            .collect::<Vec<_>>();
        if v == "off" {
            for x in &anyone {
                if let Some(pid) = text(x, "id") {
                    send(
                        &s,
                        "set_drive_file_permissions",
                        Method::DELETE,
                        &permission_url(&id, pid),
                        &[("supportsAllDrives".into(), "true".into())],
                        None,
                    )
                    .await?;
                }
            }
            changes.push(if anyone.is_empty() {
                "  - Link sharing: already off (no change)".into()
            } else {
                "  - Link sharing: disabled (restricted to specific people)".into()
            })
        } else if let Some(x) = anyone.first() {
            send(
                &s,
                "set_drive_file_permissions",
                Method::PATCH,
                &permission_url(&id, text(x, "id").unwrap_or("")),
                &[
                    ("supportsAllDrives".into(), "true".into()),
                    ("fields".into(), "id,type,role".into()),
                ],
                Some(&json!({"role":v,"allowFileDiscovery":false})),
            )
            .await?;
            changes.push(format!("  - Link sharing: updated to '{v}'"))
        } else {
            send(
                &s,
                "set_drive_file_permissions",
                Method::POST,
                &permissions_url(&id),
                &[
                    ("supportsAllDrives".into(), "true".into()),
                    ("fields".into(), "id,type,role".into()),
                ],
                Some(&json!({"type":"anyone","role":v,"allowFileDiscovery":false})),
            )
            .await?;
            changes.push(format!("  - Link sharing: enabled as '{v}'"))
        }
    }
    Ok(format!(
        "Permission settings updated for '{}'\n\nChanges:\n{}\n\nView link: {}",
        text(&m, "name").unwrap_or("Unknown"),
        changes.join("\n"),
        text(&m, "webViewLink").unwrap_or("N/A")
    ))
}

async fn auth(a: &Map<String, Value>, scope: &str) -> Result<GoogleSession, String> {
    let supplied = req_string(a, "user_google_email")?;
    let s = GoogleSession::authenticate(&[scope]).await?;
    if !supplied.eq_ignore_ascii_case(&s.user_email) {
        return Err("user_google_email does not match the configured USER_GOOGLE_EMAIL".into());
    }
    Ok(s)
}
async fn send(
    s: &GoogleSession,
    tool: &str,
    method: Method,
    url: &str,
    q: &[(String, String)],
    body: Option<&Value>,
) -> Result<Value, String> {
    google_url(url)?;
    let mut r = s.request(method, url);
    if !q.is_empty() {
        r = r.query(q)
    }
    if let Some(b) = body {
        r = r.json(b)
    }
    s.send_json(tool, r).await
}
fn google_url(v: &str) -> Result<(), String> {
    let u = Url::parse(v).map_err(|_| "Invalid Google API URL")?;
    if u.scheme() != "https"
        || u.host_str() != Some("www.googleapis.com")
        || !(u.path().starts_with("/drive/v3") || u.path().starts_with("/upload/drive/v3"))
    {
        return Err("Refusing non-Drive API host".into());
    }
    Ok(())
}
fn file_url(id: &str) -> String {
    format!("{API}/files/{}", segment(id))
}
fn permissions_url(id: &str) -> String {
    format!("{}/permissions", file_url(id))
}
fn permission_url(id: &str, pid: &str) -> String {
    format!("{}/{}", permissions_url(id), segment(pid))
}
fn segment(v: &str) -> String {
    utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()
}

struct FilesListOptions {
    page_token: Option<String>,
    drive_id: Option<String>,
    include_items_from_all_drives: bool,
    corpora: Option<String>,
    order_by: Option<String>,
    detailed: bool,
    include_permissions: bool,
}

async fn files_list(
    s: &GoogleSession,
    tool: &str,
    query: String,
    page_size: i64,
    options: FilesListOptions,
) -> Result<Value, String> {
    let fields = if options.detailed {
        format!(
            "nextPageToken,files(id,name,mimeType,webViewLink,iconLink,modifiedTime,createdTime,size,driveId,lastModifyingUser(displayName,emailAddress){})",
            if options.include_permissions {
                ",permissions(id,type,role)"
            } else {
                ""
            }
        )
    } else {
        "nextPageToken,files(id,name,mimeType)".into()
    };
    let mut params = vec![
        ("q".into(), query),
        ("pageSize".into(), page_size.to_string()),
        ("fields".into(), fields),
        ("supportsAllDrives".into(), "true".into()),
        (
            "includeItemsFromAllDrives".into(),
            options.include_items_from_all_drives.to_string(),
        ),
    ];
    push(&mut params, "pageToken", options.page_token);
    push(
        &mut params,
        "orderBy",
        options.order_by.filter(|v| !v.trim().is_empty()),
    );
    if let Some(drive_id) = options.drive_id {
        valid_id(&drive_id, "drive_id")?;
        params.push(("driveId".into(), drive_id));
        params.push(("corpora".into(), options.corpora.unwrap_or("drive".into())))
    } else {
        push(&mut params, "corpora", options.corpora)
    }
    send(s, tool, Method::GET, &format!("{API}/files"), &params, None).await
}
async fn resolve(
    s: &GoogleSession,
    tool: &str,
    input: &str,
    extra: &str,
) -> Result<(String, Value), String> {
    let mut id = valid_id(input, "file_id")?;
    let mut seen = HashSet::new();
    for depth in 0..=5 {
        if !seen.insert(id.clone()) {
            return Err(format!("Shortcut loop detected starting from '{input}'."));
        }
        let fields = if extra.is_empty() {
            "id,mimeType,parents,shortcutDetails(targetId,targetMimeType)".into()
        } else {
            format!("id,mimeType,parents,shortcutDetails(targetId,targetMimeType),{extra}")
        };
        let m = send(
            s,
            tool,
            Method::GET,
            &file_url(&id),
            &[
                ("supportsAllDrives".into(), "true".into()),
                ("fields".into(), fields),
            ],
            None,
        )
        .await?;
        if text(&m, "mimeType") != Some(SHORTCUT) {
            return Ok((id, m));
        }
        if depth == 5 {
            return Err(format!(
                "Shortcut resolution exceeded 5 hops starting from '{input}'."
            ));
        }
        id = valid_id(
            m.pointer("/shortcutDetails/targetId")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Shortcut '{id}' is missing target details."))?,
            "shortcut target",
        )?
    }
    unreachable!()
}
async fn resolve_folder(s: &GoogleSession, tool: &str, input: &str) -> Result<String, String> {
    let (id, m) = resolve(s, tool, input, "").await?;
    if text(&m, "mimeType") != Some(FOLDER) {
        return Err(format!(
            "Resolved ID '{id}' (from '{input}') is not a folder; mimeType={}.",
            text(&m, "mimeType").unwrap_or("unknown")
        ));
    }
    Ok(id)
}
async fn parents(
    s: &GoogleSession,
    tool: &str,
    input: Option<String>,
) -> Result<Option<String>, String> {
    let Some(v) = input else { return Ok(None) };
    let mut out = Vec::new();
    for x in v.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        out.push(resolve_folder(s, tool, x).await?)
    }
    Ok((!out.is_empty()).then(|| out.join(",")))
}
async fn all_permissions(s: &GoogleSession, tool: &str, id: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut token = None;
    loop {
        let mut q=vec![("supportsAllDrives".into(),"true".into()),("pageSize".into(),"100".into()),("fields".into(),"nextPageToken,permissions(id,type,role,emailAddress,displayName,domain,expirationTime,permissionDetails,allowFileDiscovery)".into())];
        push(&mut q, "pageToken", token);
        let r = send(s, tool, Method::GET, &permissions_url(id), &q, None).await?;
        out.extend(arr(&r, "permissions").iter().cloned());
        token = text(&r, "nextPageToken").map(str::to_owned);
        if token.is_none() {
            break;
        }
    }
    Ok(out)
}

async fn response(
    s: &GoogleSession,
    tool: &str,
    id: &str,
    export: Option<&str>,
) -> Result<reqwest::Response, String> {
    let (url, q) = if let Some(m) = export {
        (
            format!("{}/export", file_url(id)),
            vec![(String::from("mimeType"), String::from(m))],
        )
    } else {
        (
            file_url(id),
            vec![
                ("alt".into(), "media".into()),
                ("supportsAllDrives".into(), "true".into()),
            ],
        )
    };
    google_url(&url)?;
    let r = s
        .request(Method::GET, &url)
        .query(&q)
        .send()
        .await
        .map_err(|_| format!("API error in {tool}: Google API request failed"))?;
    if !r.status().is_success() {
        return Err(api_error(tool, r).await);
    }
    Ok(r)
}
async fn get_bytes(
    s: &GoogleSession,
    tool: &str,
    id: &str,
    export: Option<&str>,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut r = response(s, tool, id, export).await?;
    declared(&r, limit)?;
    let mut out = Vec::new();
    while let Some(c) = r
        .chunk()
        .await
        .map_err(|_| format!("API error in {tool}: failed to read response"))?
    {
        if out.len() as u64 + c.len() as u64 > limit {
            return Err(format!("Drive content exceeded the {limit} byte limit"));
        }
        out.extend_from_slice(&c)
    }
    Ok(out)
}
async fn get_file(
    s: &GoogleSession,
    tool: &str,
    id: &str,
    export: Option<&str>,
    path: &Path,
    limit: u64,
) -> Result<u64, String> {
    let mut r = response(s, tool, id, export).await?;
    declared(&r, limit)?;
    let mut f = temp_writer(path)?;
    let mut total = 0;
    while let Some(c) = r
        .chunk()
        .await
        .map_err(|_| format!("API error in {tool}: failed to read response"))?
    {
        total += c.len() as u64;
        if total > limit {
            return Err(format!("Drive download exceeded the {limit} byte limit"));
        }
        f.write_all(&c).map_err(|e| e.to_string())?
    }
    f.sync_all().map_err(|e| e.to_string())?;
    Ok(total)
}
fn declared(r: &reqwest::Response, limit: u64) -> Result<(), String> {
    if let Some(n) = r
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > limit)
    {
        return Err(format!(
            "Content exceeds the {limit} byte limit (declared {n} bytes)"
        ));
    }
    Ok(())
}
async fn api_error(tool: &str, r: reqwest::Response) -> String {
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or("Google API request failed".into());
    format!("API error in {tool}: {status}: {msg}")
}

struct Payload {
    kind: PayloadKind,
    mime: String,
    remote_mime: Option<String>,
}
enum PayloadKind {
    Bytes(Vec<u8>),
    Path(PathBuf, bool),
}
impl Payload {
    fn bytes(v: Vec<u8>, m: &str) -> Self {
        Self {
            kind: PayloadKind::Bytes(v),
            mime: m.into(),
            remote_mime: None,
        }
    }
    fn path(p: PathBuf, m: &str, delete: bool) -> Self {
        Self {
            kind: PayloadKind::Path(p, delete),
            mime: m.into(),
            remote_mime: None,
        }
    }
    fn len(&self) -> Result<u64, String> {
        match &self.kind {
            PayloadKind::Bytes(v) => Ok(v.len() as u64),
            PayloadKind::Path(p, _) => fs::metadata(p).map(|m| m.len()).map_err(|e| e.to_string()),
        }
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        if let PayloadKind::Path(p, true) = &self.kind {
            let _ = fs::remove_file(p);
        }
    }
}
async fn resumable(
    s: &GoogleSession,
    tool: &str,
    method: Method,
    id: Option<&str>,
    q: &[(String, String)],
    meta: &Value,
    p: &mut Payload,
) -> Result<Value, String> {
    let len = p.len()?;
    if len > MAX_STREAM {
        return Err(format!("Upload exceeds the {MAX_STREAM} byte limit"));
    }
    valid_mime(&p.mime)?;
    let url = id
        .map(|x| format!("{UPLOAD}/files/{}", segment(x)))
        .unwrap_or_else(|| format!("{UPLOAD}/files"));
    let mut params = q.to_vec();
    params.extend([("uploadType".into(),"resumable".into()),("supportsAllDrives".into(),"true".into()),("fields".into(),"id,name,webViewLink,mimeType,parents,description,starred,trashed,writersCanShare,copyRequiresWriterPermission,properties".into())]);
    google_url(&url)?;
    let init = s
        .request(method, &url)
        .query(&params)
        .header("X-Upload-Content-Type", &p.mime)
        .header("X-Upload-Content-Length", len)
        .json(meta)
        .send()
        .await
        .map_err(|_| format!("API error in {tool}: failed to initialize resumable upload"))?;
    if !init.status().is_success() {
        return Err(api_error(tool, init).await);
    }
    let location = init
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or("Resumable upload response omitted Location")?
        .to_owned();
    upload_location(&location)?;
    let mut file = match &p.kind {
        PayloadKind::Path(path, _) => Some(File::open(path).map_err(|e| e.to_string())?),
        _ => None,
    };
    let mut offset = 0;
    loop {
        let take = ((len - offset).min(UPLOAD_CHUNK as u64)) as usize;
        let chunk = match &p.kind {
            PayloadKind::Bytes(v) => v[offset as usize..offset as usize + take].to_vec(),
            PayloadKind::Path(_, _) => {
                let mut v = vec![0; take];
                if take > 0 {
                    file.as_mut()
                        .unwrap()
                        .read_exact(&mut v)
                        .map_err(|e| e.to_string())?
                }
                v
            }
        };
        let range = if len == 0 {
            "bytes */0".into()
        } else {
            format!("bytes {offset}-{}/{len}", offset + take as u64 - 1)
        };
        let r = s
            .request(Method::PUT, &location)
            .header(CONTENT_TYPE, &p.mime)
            .header(CONTENT_LENGTH, take)
            .header(CONTENT_RANGE, range)
            .body(chunk)
            .send()
            .await
            .map_err(|_| format!("API error in {tool}: resumable upload failed"))?;
        if r.status().is_success() {
            return r
                .json()
                .await
                .map_err(|_| format!("API error in {tool}: invalid upload response"));
        }
        if r.status().as_u16() != 308 {
            return Err(api_error(tool, r).await);
        }
        offset += take as u64;
        if offset >= len && len > 0 {
            return Err(format!("API error in {tool}: upload did not finalize"));
        }
    }
}
fn upload_location(v: &str) -> Result<(), String> {
    let u = Url::parse(v).map_err(|_| "Invalid resumable upload URL")?;
    if u.scheme() != "https"
        || u.host_str() != Some("www.googleapis.com")
        || !u.path().starts_with("/upload/drive/v3/")
    {
        return Err("Unexpected resumable upload host".into());
    }
    Ok(())
}

async fn import_payload(
    a: &Map<String, Value>,
    tool: &str,
    name: &str,
    formats: &[(&str, &str)],
    passthrough: Option<&str>,
) -> Result<Payload, String> {
    let content = string(a, "content")?;
    let path = string(a, "file_path")?;
    let url = string(a, "file_url")?;
    let count = [content.is_some(), path.is_some(), url.is_some()]
        .into_iter()
        .filter(|x| *x)
        .count();
    if count == 0 {
        return Err("You must provide one of: 'content', 'file_path', or 'file_url'.".into());
    }
    if count > 1 {
        return Err("Provide only one of: 'content', 'file_path', or 'file_url'.".into());
    }
    let hint = string(a, "source_format")?;
    let source = path.as_deref().or(url.as_deref()).unwrap_or(name);
    let mut mime = passthrough.map(str::to_owned).unwrap_or(detect(
        source,
        content.as_deref(),
        hint.as_deref(),
        formats,
    )?);
    let mut p = if let Some(v) = content {
        if !text_mime(&mime) {
            return Err(format!(
                "[{tool}] 'content' is only valid for text-based source formats"
            ));
        }
        Payload::bytes(v.into_bytes(), &mime)
    } else if let Some(v) = path {
        let actual = local_path(&v)?;
        let safe = safe_path(&actual)?;
        if passthrough.is_none() && hint.is_none() {
            mime = detect(safe.to_str().unwrap_or(name), None, None, formats)?
        }
        Payload::path(safe, &mime, false)
    } else {
        let v = url.unwrap();
        let p = remote(&v, &mime).await?;
        if passthrough.is_none() && hint.is_none() {
            if let Some(x) = p
                .remote_mime
                .as_ref()
                .filter(|x| formats.iter().any(|(_, m)| m == x))
            {
                mime = x.clone()
            } else {
                mime = detect(
                    Url::parse(&v)
                        .ok()
                        .map(|u| u.path().to_owned())
                        .as_deref()
                        .unwrap_or(&v),
                    None,
                    None,
                    formats,
                )?
            }
        }
        p
    };
    if passthrough.is_none() && !formats.iter().any(|(_, m)| *m == mime) {
        return Err(format!(
            "[{tool}] Detected source MIME type '{mime}' is not supported"
        ));
    }
    p.mime = mime;
    Ok(p)
}
fn detect(
    name: &str,
    content: Option<&str>,
    hint: Option<&str>,
    formats: &[(&str, &str)],
) -> Result<String, String> {
    if let Some(h) = hint {
        let h = h.trim_start_matches('.').to_lowercase();
        return formats
            .iter()
            .find(|(e, _)| *e == h)
            .map(|(_, m)| (*m).into())
            .ok_or_else(|| format!("Unsupported source_format: '{h}'"));
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_lowercase();
    if let Some((_, m)) = formats.iter().find(|(e, _)| *e == ext) {
        return Ok((*m).into());
    }
    if let Some(c) = content {
        let m = if c.starts_with('#') || c.contains("**") {
            "text/markdown"
        } else {
            "text/plain"
        };
        if formats.iter().any(|(_, x)| *x == m) {
            return Ok(m.into());
        }
    }
    Err(format!(
        "Detected source format from '{name}' is not supported"
    ))
}
fn text_mime(m: &str) -> bool {
    m.starts_with("text/")
        || matches!(
            m,
            "application/rtf" | "application/json" | "application/xml"
        )
        || m.ends_with("+json")
        || m.ends_with("+xml")
}
async fn url_payload(v: &str, mime: &str, files: bool) -> Result<Payload, String> {
    if v.starts_with("file:") {
        if !files {
            return Err("Remote URL must use http:// or https://".into());
        }
        let path = safe_path(&local_path(v)?)?;
        return Ok(Payload::path(path, mime, false));
    }
    if !v.contains("://") {
        return Err("fileUrl is missing a URL scheme".into());
    }
    remote(v, mime).await
}

async fn remote(v: &str, mime: &str) -> Result<Payload, String> {
    let tmp = Temp::new("drive_url")?;
    let mut url = Url::parse(v).map_err(|_| "Invalid remote URL")?;
    for _ in 0..10 {
        valid_remote(&url)?;
        let ips = public_ips(&url).await?;
        let mut response = None;
        for ip in ips {
            let host = url.host_str().unwrap().to_owned();
            let port = url.port_or_known_default().unwrap();
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(30))
                .resolve(&host, SocketAddr::new(ip, port))
                .build()
                .map_err(|e| e.to_string())?;
            if let Ok(r) = client.get(url.clone()).send().await {
                response = Some(r);
                break;
            }
        }
        let mut r = response.ok_or_else(|| format!("Failed to fetch {}", redact(&url)))?;
        if [301, 302, 303, 307, 308].contains(&r.status().as_u16()) {
            let loc = r
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("Redirect missing Location")?;
            url = url.join(loc).map_err(|_| "Invalid redirect URL")?;
            continue;
        }
        if r.status() != StatusCode::OK {
            return Err(format!(
                "Failed to fetch {} (status {})",
                redact(&url),
                r.status()
            ));
        }
        declared(&r, MAX_STREAM)?;
        let remote_mime = remote_mime(r.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()));
        let mut f = temp_writer(&tmp.path)?;
        let mut total = 0;
        while let Some(c) = r
            .chunk()
            .await
            .map_err(|_| format!("Failed to fetch {}", redact(&url)))?
        {
            total += c.len() as u64;
            if total > MAX_STREAM {
                return Err(format!("Remote file exceeded {MAX_STREAM} bytes"));
            }
            f.write_all(&c).map_err(|e| e.to_string())?
        }
        let path = tmp.path.clone();
        tmp.disarm();
        let mut p = Payload::path(path, mime, true);
        p.remote_mime = remote_mime;
        return Ok(p);
    }
    Err("Too many redirects".into())
}
fn valid_remote(u: &Url) -> Result<(), String> {
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!(
            "Only http:// and https:// are supported: {}",
            redact(u)
        ));
    }
    if u.host_str().is_none() {
        return Err("Invalid URL: no hostname".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("URL credentials are not allowed".into());
    }
    Ok(())
}
async fn public_ips(u: &Url) -> Result<Vec<IpAddr>, String> {
    let host = u.host_str().ok_or("Invalid URL: no hostname")?.to_owned();
    let port = u.port_or_known_default().ok_or("Invalid port")?;
    let lookup = host.clone();
    let ips = tokio::task::spawn_blocking(move || {
        (lookup.as_str(), port)
            .to_socket_addrs()
            .map(|i| i.map(|x| x.ip()).collect::<Vec<_>>())
    })
    .await
    .map_err(|_| format!("Cannot resolve hostname '{host}'"))?
    .map_err(|e| format!("Cannot resolve hostname '{host}': {e}"))?;
    if ips.is_empty() {
        return Err(format!("No addresses found for hostname: {host}"));
    }
    let mut out = Vec::new();
    for ip in ips {
        if blocked(ip) {
            return Err(format!(
                "URLs pointing to private/internal networks are not allowed: {host} resolves to {ip}"
            ));
        }
        if !out.contains(&ip) {
            out.push(ip)
        }
    }
    Ok(out)
}
fn blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => blocked4(v),
        IpAddr::V6(v) => v
            .to_ipv4_mapped()
            .map(blocked4)
            .unwrap_or_else(|| blocked6(v)),
    }
}
fn blocked4(ip: Ipv4Addr) -> bool {
    let x = u32::from(ip);
    [
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
    .any(|(n, p)| cidr4(x, n, p))
}
fn cidr4(x: u32, n: &str, p: &u32) -> bool {
    let n = u32::from(n.parse::<Ipv4Addr>().unwrap());
    let mask = u32::MAX << (32 - *p);
    x & mask == n & mask
}
fn blocked6(ip: Ipv6Addr) -> bool {
    let x = u128::from(ip);
    [
        ("::", 96),
        ("64:ff9b::", 96),
        ("64:ff9b:1::", 48),
        ("100::", 64),
        ("2001::", 23),
        ("2001:db8::", 32),
        ("2002::", 16),
        ("3fff::", 20),
        ("5f00::", 16),
        ("fc00::", 7),
        ("fec0::", 10),
        ("fe80::", 10),
        ("ff00::", 8),
    ]
    .iter()
    .any(|(n, p)| {
        let n = u128::from(n.parse::<Ipv6Addr>().unwrap());
        let mask = u128::MAX << (128 - *p);
        x & mask == n & mask
    })
}
fn redact(u: &Url) -> String {
    format!("{}{}", u.host_str().unwrap_or("<redacted>"), u.path())
}
fn remote_mime(v: Option<&str>) -> Option<String> {
    let m = v?.split(';').next()?.trim().to_lowercase();
    if m == "application/octet-stream"
        || m.starts_with("application/vnd.google-apps")
        || valid_mime(&m).is_err()
    {
        None
    } else {
        Some(m)
    }
}

fn local_path(v: &str) -> Result<String, String> {
    if !v.starts_with("file:") {
        if v.contains("://") {
            return Err(format!(
                "file_path should be a local path or file:// URL, got: {v}"
            ));
        }
        return Ok(v.into());
    }
    let u = Url::parse(v).map_err(|_| "Invalid file URL")?;
    if u.host_str()
        .is_some_and(|h| !h.is_empty() && !h.eq_ignore_ascii_case("localhost"))
    {
        return Err("Remote file:// authorities are not supported".into());
    }
    percent_decode_str(u.path())
        .decode_utf8()
        .map(|v| v.into_owned())
        .map_err(|_| "Invalid file path encoding".into())
}
fn safe_path(v: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(v);
    let candidate = if raw.is_absolute() {
        raw
    } else {
        env::current_dir().map_err(|e| e.to_string())?.join(raw)
    };
    deny(&candidate)?;
    let resolved = fs::canonicalize(&candidate).map_err(|_| {
        format!(
            "Access to '{}' is not allowed or path does not exist",
            candidate.display()
        )
    })?;
    deny(&resolved)?;
    let allowed = allowed_dirs();
    if !allowed.iter().any(|p| resolved.starts_with(p)) {
        return Err(format!(
            "Access to '{}' is not allowed: outside permitted directories ({})",
            resolved.display(),
            allowed
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let m = fs::metadata(&resolved).map_err(|e| e.to_string())?;
    if !m.is_file() {
        return Err(format!("Path is not a file: {v}"));
    }
    if m.len() > MAX_STREAM {
        return Err(format!("Local file exceeds the {MAX_STREAM} byte limit"));
    }
    Ok(resolved)
}
fn allowed_dirs() -> Vec<PathBuf> {
    let mut v = vec![
        env::var_os("WORKSPACE_ATTACHMENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                home()
                    .unwrap_or_else(env::temp_dir)
                    .join(".workspace-mcp/attachments")
            }),
    ];
    if let Some(x) = env::var_os("ALLOWED_FILE_DIRS") {
        v.extend(env::split_paths(&x))
    }
    v.into_iter().map(normalized).collect()
}
fn normalized(p: PathBuf) -> PathBuf {
    if let Ok(p) = fs::canonicalize(&p) {
        return p;
    }
    let p = if p.is_absolute() {
        p
    } else {
        env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    out
}
fn deny(p: &Path) -> Result<(), String> {
    let s = p.to_string_lossy().to_lowercase().replace('\\', "/");
    let parts = p
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let name = p
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    let secret = parts.iter().any(|x| {
        x == ".env"
            || x.starts_with(".env.")
            || matches!(x.as_str(), ".ssh" | ".aws" | ".kube" | ".gnupg")
    }) || [
        "/proc",
        "/sys",
        "/dev",
        "/etc/passwd",
        "/etc/shadow",
        "/private/etc/passwd",
        "/private/etc/shadow",
    ]
    .iter()
    .any(|x| s == *x || s.starts_with(&format!("{x}/")))
        || s.contains("/.config/gcloud/")
        || [
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
        .contains(&name.as_str())
        || s.ends_with("/.docker/config.json");
    if secret {
        Err(format!(
            "Access to '{}' is not allowed: path may contain secrets",
            p.display()
        ))
    } else {
        Ok(())
    }
}

struct Temp {
    path: PathBuf,
    armed: std::cell::Cell<bool>,
}
impl Temp {
    fn new(prefix: &str) -> Result<Self, String> {
        for _ in 0..16 {
            let p = env::temp_dir().join(format!("{prefix}_{}", token()));
            if secure_file(&p).is_ok() {
                return Ok(Self {
                    path: p,
                    armed: std::cell::Cell::new(true),
                });
            }
        }
        Err("Failed to create temporary file".into())
    }
    fn disarm(&self) {
        self.armed.set(false)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn secure_file(p: &Path) -> Result<File, String> {
    let mut o = OpenOptions::new();
    o.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p)
        .map_err(|e| format!("Failed to create {}: {e}", p.display()))
}
fn temp_writer(p: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(p)
        .map_err(|e| format!("Failed to open secure temporary file {}: {e}", p.display()))
}
struct Saved {
    id: String,
    path: PathBuf,
}
fn save_attachment(src: &Path, name: &str, owner: &str) -> Result<Saved, String> {
    if owner.trim().is_empty() {
        return Err("Attachment owner required".into());
    }
    let base = env::var_os("WORKSPACE_ATTACHMENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home()
                .unwrap_or_else(env::temp_dir)
                .join(".workspace-mcp/attachments")
        });
    dir(&base)?;
    let mut h = DefaultHasher::new();
    owner.trim().to_lowercase().hash(&mut h);
    let base = base.join(format!("owner_{:x}", h.finish()));
    dir(&base)?;
    let id = token();
    let safe = filename(name);
    let p = Path::new(&safe);
    let stem = p
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("attachment");
    let ext = p
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| format!(".{x}"))
        .unwrap_or_default();
    let dest = base.join(format!("{stem}_{id}{ext}"));
    if fs::rename(src, &dest).is_err() {
        let mut i = File::open(src).map_err(|e| e.to_string())?;
        let mut o = secure_file(&dest)?;
        std::io::copy(&mut i, &mut o).map_err(|e| e.to_string())?;
        fs::remove_file(src).map_err(|e| e.to_string())?
    }
    mode(&dest, 0o600)?;
    Ok(Saved {
        id,
        path: fs::canonicalize(dest).map_err(|e| e.to_string())?,
    })
}
fn dir(p: &Path) -> Result<(), String> {
    fs::create_dir_all(p).map_err(|e| e.to_string())?;
    mode(p, 0o700)
}
fn mode(p: &Path, m: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(m)).map_err(|e| e.to_string())?
    }
    Ok(())
}
fn filename(v: &str) -> String {
    let s = v
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    let s = s.trim_end_matches([' ', '.']);
    if s.is_empty() {
        "attachment".into()
    } else {
        s.into()
    }
}
fn token() -> String {
    format!(
        "{:x}{:x}{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}
fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
fn stateless() -> bool {
    truth("WORKSPACE_MCP_STATELESS_MODE") || truth("MCP_STATELESS_MODE")
}
fn truth(k: &str) -> bool {
    env::var(k).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}
fn transport() -> String {
    env::var("WORKSPACE_MCP_TRANSPORT")
        .or_else(|_| env::var("MCP_TRANSPORT"))
        .unwrap_or("stdio".into())
}
fn attachment_url(id: &str) -> Result<String, String> {
    let base = env::var("WORKSPACE_EXTERNAL_URL")
        .or_else(|_| env::var("WORKSPACE_MCP_BASE_URI"))
        .map_err(|_| "Attachment base URL is not configured")?;
    Ok(format!(
        "{}/attachments/{}",
        base.trim_end_matches('/'),
        segment(id)
    ))
}

static LOCKS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
struct Lock(String);
impl Lock {
    fn take(id: &str) -> Result<Self, String> {
        let mut g = LOCKS
            .get_or_init(|| std::sync::Mutex::new(HashSet::new()))
            .lock()
            .map_err(|_| "Content lock failed")?;
        if !g.insert(id.into()) {
            return Err(format!("Another update is in progress for '{id}'"));
        }
        Ok(Self(id.into()))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(x) = LOCKS.get()
            && let Ok(mut g) = x.lock()
        {
            g.remove(&self.0);
        }
    }
}

fn validate(a: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for (k, v) in a {
        let c = allowed
            .iter()
            .copied()
            .find(|c| matches_key(k, c))
            .ok_or_else(|| format!("Unexpected argument: {k}"))?;
        if !seen.insert(c) {
            return Err(format!("Unexpected argument: {k}"));
        }
        if v.is_null() {
            continue;
        }
        let bools = [
            "include_items_from_all_drives",
            "detailed",
            "include_trashed",
            "include_organizers",
            "starred",
            "trashed",
            "writers_can_share",
            "copy_requires_writer_permission",
            "send_notification",
            "allow_file_discovery",
            "move_to_new_owners_root",
        ];
        let ok = if c == "page_size" {
            v.is_i64()
        } else if bools.contains(&c) {
            v.is_boolean()
        } else if c == "properties" {
            v.is_object()
                || v.as_str()
                    .is_some_and(|x| serde_json::from_str::<Map<String, Value>>(x).is_ok())
        } else if c == "recipients" {
            v.is_array()
                || v.as_str()
                    .is_some_and(|x| serde_json::from_str::<Vec<Value>>(x).is_ok())
        } else {
            v.is_string()
        };
        if !ok {
            return Err(format!("{c} has an invalid type"));
        }
    }
    Ok(())
}
fn matches_key(k: &str, c: &str) -> bool {
    k == c
        || if c == "fileUrl" {
            k == "file_url"
        } else {
            k == camel(c)
        }
}
fn camel(v: &str) -> String {
    let mut o = String::new();
    let mut up = false;
    for c in v.chars() {
        if c == '_' {
            up = true
        } else if up {
            o.extend(c.to_uppercase());
            up = false
        } else {
            o.push(c)
        }
    }
    o
}
fn arg<'a>(a: &'a Map<String, Value>, n: &str) -> Option<&'a Value> {
    a.iter().find(|(k, _)| matches_key(k, n)).map(|x| x.1)
}
fn string(a: &Map<String, Value>, n: &str) -> Result<Option<String>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        _ => Err(format!("{n} must be a string")),
    }
}
fn req_string(a: &Map<String, Value>, n: &str) -> Result<String, String> {
    string(a, n)?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{n} is required"))
}
fn or_string(a: &Map<String, Value>, n: &str, d: &str) -> Result<String, String> {
    Ok(string(a, n)?.unwrap_or(d.into()))
}
fn integer(a: &Map<String, Value>, n: &str, d: i64) -> Result<i64, String> {
    match arg(a, n) {
        None => Ok(d),
        Some(v) => v.as_i64().ok_or_else(|| format!("{n} must be an integer")),
    }
}
fn optional_bool(a: &Map<String, Value>, n: &str) -> Result<Option<bool>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(v)) => Ok(Some(*v)),
        _ => Err(format!("{n} must be a boolean")),
    }
}
fn boolean(a: &Map<String, Value>, n: &str, d: bool) -> Result<bool, String> {
    Ok(optional_bool(a, n)?.unwrap_or(d))
}
fn object(a: &Map<String, Value>, n: &str) -> Result<Option<Map<String, Value>>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(v)) => Ok(Some(v.clone())),
        Some(Value::String(v)) => serde_json::from_str(v)
            .map(Some)
            .map_err(|_| format!("{n} must be an object or JSON object string")),
        _ => Err(format!("{n} must be an object")),
    }
}
fn array_arg(a: &Map<String, Value>, n: &str) -> Result<Option<Vec<Value>>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(v)) => Ok(Some(v.clone())),
        Some(Value::String(v)) => serde_json::from_str(v)
            .map(Some)
            .map_err(|_| format!("{n} must be an array or JSON array string")),
        _ => Err(format!("{n} must be an array")),
    }
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}
fn flag(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}
fn value(v: Option<&Value>, d: &str) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(x)) => x.to_string(),
        _ => d.into(),
    }
}
fn push(q: &mut Vec<(String, String)>, k: &str, v: Option<String>) {
    if let Some(v) = v.filter(|x| !x.is_empty()) {
        q.push((k.into(), v))
    }
}
fn next(o: &mut Vec<String>, v: &Value) {
    if let Some(x) = text(v, "nextPageToken") {
        o.push(format!("nextPageToken: {x}"))
    }
}
fn valid_id(v: &str, n: &str) -> Result<String, String> {
    let v = v.trim();
    if v.is_empty()
        || v.len() > 256
        || !v
            .bytes()
            .all(|x| x.is_ascii_alphanumeric() || x == b'-' || x == b'_')
    {
        Err(format!("{n} is not a valid Drive ID: '{v}'"))
    } else {
        Ok(v.into())
    }
}
fn valid_mime(v: &str) -> Result<(), String> {
    let Some((a, b)) = v.split_once('/') else {
        return Err(format!("Invalid MIME type '{v}'"));
    };
    if a.is_empty()
        || b.is_empty()
        || b.contains('/')
        || !a
            .bytes()
            .chain(b.bytes())
            .all(|x| x.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&x))
    {
        Err(format!("Invalid MIME type '{v}'"))
    } else {
        Ok(())
    }
}
fn file_type(v: &str) -> Result<String, String> {
    let n = v.trim().to_lowercase();
    let m = match n.as_str() {
        "folder" | "folders" => FOLDER,
        "document" | "doc" | "documents" | "docs" => DOC,
        "spreadsheet" | "sheet" | "spreadsheets" | "sheets" => SHEET,
        "presentation" | "presentations" | "slide" | "slides" => SLIDE,
        "form" | "forms" => "application/vnd.google-apps.form",
        "drawing" | "drawings" => "application/vnd.google-apps.drawing",
        "pdf" | "pdfs" => "application/pdf",
        "shortcut" | "shortcuts" => SHORTCUT,
        "script" | "scripts" => "application/vnd.google-apps.script",
        "site" | "sites" => "application/vnd.google-apps.site",
        "jam" | "jamboard" | "jamboards" => "application/vnd.google-apps.jam",
        _ if n.contains('/') => {
            valid_mime(&n)?;
            return Ok(n);
        }
        _ => return Err(format!("Unknown file_type '{v}'")),
    };
    Ok(m.into())
}
fn esc(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\'', "\\'")
}
fn query_syntax(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut quote = None;
    let mut escaped = false;
    for c in v.chars() {
        if escaped {
            escaped = false;
            if quote.is_none() {
                out.push(c);
            }
            continue;
        }
        if c == '\\' {
            escaped = true;
            if quote.is_none() {
                out.push(c);
            }
            continue;
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => out.push(' '),
            None if c == '\'' || c == '"' => quote = Some(c),
            None => out.push(c),
        }
    }
    out
}
fn structured(v: &str) -> bool {
    let v = query_syntax(v).to_lowercase();
    [" contains ", " in parents", " has {", "=", "!=", ">", "<"]
        .iter()
        .any(|x| v.contains(x))
}
fn trashed_clause(v: &str) -> bool {
    let x = query_syntax(v).to_lowercase().replace(' ', "");
    [
        "trashed=true",
        "trashed=false",
        "trashed!=true",
        "trashed!=false",
    ]
    .iter()
    .any(|s| x.contains(s))
}
fn format_file(v: &Value, detailed: bool, permissions: bool) -> String {
    let (name, id, mime) = (
        text(v, "name").unwrap_or(""),
        text(v, "id").unwrap_or(""),
        text(v, "mimeType").unwrap_or(""),
    );
    if !detailed {
        return format!("- Name: \"{name}\" (ID: {id}, Type: {mime})");
    }
    let mut e = Vec::new();
    if let Some(x) = v.get("size") {
        e.push(format!("Size: {}", value(Some(x), "N/A")))
    }
    if let Some(x) = text(v, "createdTime") {
        e.push(format!("Created: {x}"))
    }
    e.push(format!(
        "Modified: {}",
        text(v, "modifiedTime").unwrap_or("N/A")
    ));
    if permissions
        && let Some(p) = arr(v, "permissions")
            .iter()
            .find(|p| text(p, "type") == Some("anyone"))
    {
        e.push(format!(
            "Anyone with link: {}",
            text(p, "role").unwrap_or("unknown")
        ))
    }
    if let Some(x) = text(v, "driveId") {
        e.push(format!("Drive ID: {x}"))
    }
    format!(
        "- Name: \"{name}\" (ID: {id}, Type: {mime}, {}) Link: {}",
        e.join(", "),
        text(v, "webViewLink").unwrap_or("#")
    )
}
fn true_keys(v: Option<&Value>) -> String {
    let x = v
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter(|x| x.1.as_bool() == Some(true))
                .map(|x| x.0.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if x.is_empty() {
        "none".into()
    } else {
        x.join(", ")
    }
}
fn is_public(p: &[Value]) -> bool {
    p.iter().any(|x| {
        text(x, "type") == Some("anyone")
            && matches!(text(x, "role"), Some("reader" | "writer" | "commenter"))
    })
}
fn is_shared(m: &Value, p: &[Value]) -> bool {
    flag(m, "shared").unwrap_or(false)
        || m.get("driveId").is_some()
        || p.iter()
            .any(|x| matches!(text(x, "type"), Some("anyone" | "domain")))
        || p.iter()
            .filter(|x| matches!(text(x, "type"), Some("user" | "group")))
            .count()
            > 1
}
fn perm(p: &Value) -> String {
    let (kind, role, id) = (
        text(p, "type").unwrap_or("unknown"),
        text(p, "role").unwrap_or("unknown"),
        text(p, "id").unwrap_or(""),
    );
    match kind {
        "anyone" => format!("Anyone with the link ({role}) [id: {id}]"),
        "user" => format!(
            "User: {} ({role}) [id: {id}]",
            text(p, "emailAddress").unwrap_or("unknown")
        ),
        "group" => format!(
            "Group: {} ({role}) [id: {id}]",
            text(p, "emailAddress").unwrap_or("unknown")
        ),
        "domain" => format!(
            "Domain: {} ({role}) [id: {id}]",
            text(p, "domain").unwrap_or("unknown")
        ),
        _ => format!("{kind} ({role}) [id: {id}]"),
    }
}
fn valid_role(v: &str) -> Result<(), String> {
    if ["reader", "commenter", "writer"].contains(&v) {
        Ok(())
    } else {
        Err(format!(
            "Invalid role '{v}'. Must be one of: commenter, reader, writer"
        ))
    }
}
fn valid_type(v: &str) -> Result<(), String> {
    if ["user", "group", "domain", "anyone"].contains(&v) {
        Ok(())
    } else {
        Err(format!("Invalid share_type '{v}'"))
    }
}
fn valid_exp(v: &str) -> Result<(), String> {
    OffsetDateTime::parse(v, &Rfc3339)
        .map(|_| ())
        .map_err(|_| format!("Invalid expiration_time '{v}'. Must be RFC 3339 format"))
}
fn perm_body(
    kind: &str,
    role: &str,
    who: Option<&str>,
    exp: Option<&str>,
    discovery: Option<bool>,
) -> Result<Value, String> {
    valid_role(role)?;
    valid_type(kind)?;
    if ["user", "group", "domain"].contains(&kind) && who.is_none_or(str::is_empty) {
        return Err(format!("share_with is required for share_type '{kind}'"));
    }
    let mut m = Map::new();
    m.insert("type".into(), Value::String(kind.into()));
    m.insert("role".into(), Value::String(role.into()));
    if ["user", "group"].contains(&kind) {
        m.insert("emailAddress".into(), Value::String(who.unwrap().into()));
    } else if kind == "domain" {
        m.insert("domain".into(), Value::String(who.unwrap().into()));
    }
    if let Some(e) = exp {
        valid_exp(e)?;
        m.insert("expirationTime".into(), Value::String(e.into()));
    }
    if let Some(v) = discovery.filter(|_| matches!(kind, "domain" | "anyone")) {
        m.insert("allowFileDiscovery".into(), Value::Bool(v));
    }
    Ok(Value::Object(m))
}
fn splice(a: &str, b: &str, mode: &str) -> String {
    let (x, y) = if mode == "append" { (a, b) } else { (b, a) };
    format!(
        "{x}{}{y}",
        if !x.is_empty() && !x.ends_with('\n') && !y.starts_with('\n') {
            "\n"
        } else {
            ""
        }
    )
}
fn stem(v: &str) -> String {
    Path::new(v)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or(v)
        .into()
}
fn export_choice<'a>(mime: &str, name: &str, f: &str) -> (Option<&'a str>, String, String) {
    let (ext, out, e) = match mime {
        DOC if f == "docx" => (
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        ),
        DOC => ("pdf", "application/pdf", Some("application/pdf")),
        SHEET if f == "csv" => ("csv", "text/csv", Some("text/csv")),
        SHEET if f == "pdf" => ("pdf", "application/pdf", Some("application/pdf")),
        SHEET => (
            "xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        ),
        SLIDE if f == "pptx" => (
            "pptx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        ),
        SLIDE => ("pdf", "application/pdf", Some("application/pdf")),
        _ => ("", mime, None),
    };
    (
        e,
        out.into(),
        if ext.is_empty() {
            name.into()
        } else {
            format!("{}.{}", stem(name), ext)
        },
    )
}

fn max_b64(n: usize) -> usize {
    n.div_ceil(3) * 4
}
fn b64_encode(v: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(max_b64(v.len()));
    for c in v.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((c.get(1).copied().unwrap_or(0) as u32) << 8)
            | c.get(2).copied().unwrap_or(0) as u32;
        o.push(T[((n >> 18) & 63) as usize] as char);
        o.push(T[((n >> 12) & 63) as usize] as char);
        o.push(if c.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        o.push(if c.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        })
    }
    o
}
fn b64_decode(v: &str) -> Result<Vec<u8>, String> {
    if !v.len().is_multiple_of(4) {
        return Err("'base64_content' must be valid standard base64.".into());
    }
    let mut out = Vec::new();
    for (i, c) in v.as_bytes().chunks(4).enumerate() {
        let last = i + 1 == v.len() / 4;
        let p2 = c[2] == b'=';
        let p3 = c[3] == b'=';
        if p2 && !p3 || !last && (p2 || p3) {
            return Err("'base64_content' must be valid standard base64.".into());
        }
        let (a, b, x, y) = (
            b64(c[0])?,
            b64(c[1])?,
            if p2 { 0 } else { b64(c[2])? },
            if p3 { 0 } else { b64(c[3])? },
        );
        let n = ((a as u32) << 18) | ((b as u32) << 12) | ((x as u32) << 6) | y as u32;
        out.push((n >> 16) as u8);
        if !p2 {
            out.push((n >> 8) as u8)
        }
        if !p3 {
            out.push(n as u8)
        }
    }
    Ok(out)
}
fn b64(v: u8) -> Result<u8, String> {
    match v {
        b'A'..=b'Z' => Ok(v - b'A'),
        b'a'..=b'z' => Ok(v - b'a' + 26),
        b'0'..=b'9' => Ok(v - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err("'base64_content' must be valid standard base64.".into()),
    }
}
