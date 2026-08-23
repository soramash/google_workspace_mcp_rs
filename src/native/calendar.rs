use crate::auth::GoogleSession;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime};

const API: &str = "https://www.googleapis.com/calendar/v3";
const DRIVE_API: &str = "https://www.googleapis.com/drive/v3";
const READ: &str = "https://www.googleapis.com/auth/calendar.readonly";
const EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
const CALENDAR: &str = "https://www.googleapis.com/auth/calendar";
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

const LIST_ARGS: &[&str] = &["user_google_email"];
const GET_ARGS: &[&str] = &[
    "user_google_email",
    "calendar_id",
    "event_id",
    "time_min",
    "time_max",
    "max_results",
    "query",
    "detailed",
    "include_attachments",
];
const EVENT_ARGS: &[&str] = &[
    "user_google_email",
    "action",
    "summary",
    "start_time",
    "end_time",
    "event_id",
    "calendar_id",
    "description",
    "location",
    "attendees",
    "timezone",
    "attachments",
    "add_google_meet",
    "conference_data",
    "conference_provider",
    "conference_uri",
    "conference_passcode",
    "conference_id",
    "reminders",
    "use_default_reminders",
    "transparency",
    "visibility",
    "color_id",
    "recurrence",
    "guests_can_modify",
    "guests_can_invite_others",
    "guests_can_see_other_guests",
    "response",
    "rsvp_comment",
    "send_updates",
];
const OOO_ARGS: &[&str] = &[
    "user_google_email",
    "action",
    "start_time",
    "end_time",
    "summary",
    "auto_decline_mode",
    "decline_message",
    "recurrence",
    "timezone",
    "time_min",
    "time_max",
    "max_results",
    "event_id",
    "calendar_id",
];
const FOCUS_ARGS: &[&str] = &[
    "user_google_email",
    "action",
    "start_time",
    "end_time",
    "summary",
    "description",
    "auto_decline_mode",
    "decline_message",
    "chat_status",
    "recurrence",
    "timezone",
    "time_min",
    "time_max",
    "max_results",
    "event_id",
    "calendar_id",
];
const FREEBUSY_ARGS: &[&str] = &[
    "user_google_email",
    "time_min",
    "time_max",
    "calendar_ids",
    "group_expansion_max",
    "calendar_expansion_max",
];
const CREATE_CALENDAR_ARGS: &[&str] = &["user_google_email", "summary", "description", "timezone"];

pub async fn dispatch(tool_name: &str, arguments: &Map<String, Value>) -> Result<String, String> {
    match tool_name {
        "list_calendars" => list_calendars(arguments).await,
        "get_events" => get_events(arguments).await,
        "manage_event" => manage_event(arguments).await,
        "manage_out_of_office" => manage_special(arguments, Special::Ooo).await,
        "manage_focus_time" => manage_special(arguments, Special::Focus).await,
        "query_freebusy" => query_freebusy(arguments).await,
        "create_calendar" => create_calendar(arguments).await,
        _ => Err(format!("Unsupported native Calendar tool: {tool_name}")),
    }
}

async fn list_calendars(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, LIST_ARGS)?;
    let s = auth(a, READ).await?;
    let r = send(
        &s,
        "list_calendars",
        Method::GET,
        &format!("{API}/users/me/calendarList"),
        &[],
        None,
    )
    .await?;
    let items = arr(&r, "items");
    if items.is_empty() {
        return Ok(format!("No calendars found for {}.", s.user_email));
    }
    let mut lines = Vec::new();
    for item in items {
        lines.push(format!(
            "- \"{}\"{} (ID: {})",
            text(item, "summary").unwrap_or("No Summary"),
            if flag(item, "primary").unwrap_or(false) {
                " (Primary)"
            } else {
                ""
            },
            required_api(item, "id")?
        ));
    }
    Ok(format!(
        "Successfully listed {} calendars for {}:\n{}",
        lines.len(),
        s.user_email,
        lines.join("\n")
    ))
}

async fn get_events(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, GET_ARGS)?;
    let cal = string_or(a, "calendar_id", "primary")?;
    let id = opt_string(a, "event_id")?.filter(|value| !value.is_empty());
    let detailed = bool_or(a, "detailed", false)?;
    let attachments = bool_or(a, "include_attachments", false)?;
    let s = auth(a, READ).await?;
    let items = if let Some(id) = id.as_deref().filter(|v| !v.is_empty()) {
        vec![
            send(
                &s,
                "get_events",
                Method::GET,
                &event_url(&cal, id),
                &[],
                None,
            )
            .await?,
        ]
    } else {
        let mut q = vec![
            (
                "timeMin".into(),
                normalize_time(opt_string(a, "time_min")?.as_deref())?.unwrap_or_else(now),
            ),
            (
                "maxResults".into(),
                int_or(a, "max_results", 25)?.to_string(),
            ),
            ("singleEvents".into(), "true".into()),
            ("orderBy".into(), "startTime".into()),
        ];
        push(
            &mut q,
            "timeMax",
            normalize_time(opt_string(a, "time_max")?.as_deref())?.as_deref(),
        );
        push(&mut q, "q", opt_string(a, "query")?.as_deref());
        let r = send(&s, "get_events", Method::GET, &events_url(&cal), &q, None).await?;
        arr(&r, "items").to_vec()
    };
    if items.is_empty() {
        return Ok(if let Some(id) = id {
            format!(
                "Event with ID '{id}' not found in calendar '{cal}' for {}.",
                s.user_email
            )
        } else {
            format!(
                "No events found in calendar '{cal}' for {} for the specified time range.",
                s.user_email
            )
        });
    }
    if let Some(id) = id.as_deref().filter(|_| detailed) {
        let e = &items[0];
        return Ok(format!(
            "Event Details:\n- Title: {}\n- Starts: {}\n- Ends: {}\n{}- Event ID: {id}\n- Link: {}",
            text(e, "summary").unwrap_or("No Title"),
            format_time(e, "start"),
            format_time(e, "end"),
            detail_lines(e, "- ", "  ", attachments),
            text(e, "htmlLink").unwrap_or("No Link")
        ));
    }
    let lines: Vec<String> = items
        .iter()
        .map(|e| {
            let title = text(e, "summary").unwrap_or("No Title");
            let start = format_time(e, "start");
            let end = format_time(e, "end");
            let eid = text(e, "id").unwrap_or("No ID");
            let link = text(e, "htmlLink").unwrap_or("No Link");
            if detailed {
                format!(
                    "- \"{title}\" (Starts: {start}, Ends: {end})\n{}  ID: {eid} | Link: {link}",
                    detail_lines(e, "  ", "    ", attachments)
                )
            } else {
                format!(
                    "- \"{title}\" (Starts: {start}, Ends: {end}){} ID: {eid} | Link: {link}",
                    meeting_link(e)
                        .map(|v| format!(" Meeting: {v}"))
                        .unwrap_or_default()
                )
            }
        })
        .collect();
    let head = if id.is_some() {
        format!(
            "Successfully retrieved event from calendar '{cal}' for {}:\n",
            s.user_email
        )
    } else {
        format!(
            "Successfully retrieved {} events from calendar '{cal}' for {}:\n",
            items.len(),
            s.user_email
        )
    };
    Ok(head + &lines.join("\n"))
}

async fn manage_event(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, EVENT_ARGS)?;
    let action = required(a, "action")?.trim().to_lowercase();
    let updates = opt_string(a, "send_updates")?.unwrap_or_else(|| "all".into());
    if !matches!(updates.as_str(), "all" | "externalOnly" | "none") {
        return Err(format!(
            "Invalid send_updates '{updates}'. Must be one of: ['all', 'externalOnly', 'none']"
        ));
    }
    let s = auth(a, EVENTS).await?;
    match action.as_str() {
        "create" => create_event(&s, a, &updates).await,
        "update" => update_event(&s, a, &updates).await,
        "delete" => delete_event(&s, a, &updates).await,
        "rsvp" => rsvp_event(&s, a, &updates).await,
        _ => Err(format!(
            "Invalid action '{action}'. Must be 'create', 'update', 'delete', or 'rsvp'."
        )),
    }
}

async fn create_event(
    s: &GoogleSession,
    a: &Map<String, Value>,
    updates: &str,
) -> Result<String, String> {
    let summary = opt_string(a, "summary")?.filter(|v| !v.is_empty());
    let start = opt_string(a, "start_time")?.filter(|v| !v.is_empty());
    let end = opt_string(a, "end_time")?.filter(|v| !v.is_empty());
    if summary.is_none() || start.is_none() || end.is_none() {
        return Err("summary, start_time, and end_time are required for create action".into());
    }
    let summary = summary.unwrap_or_default();
    let start = start.unwrap_or_default();
    let end = end.unwrap_or_default();
    let cal = string_or(a, "calendar_id", "primary")?;
    let tz = opt_string(a, "timezone")?;
    let add_meet = opt_bool(a, "add_google_meet")?.unwrap_or(false);
    let conference = conference(a, add_meet)?;
    let mut b = Map::new();
    b.insert("summary".into(), json!(summary));
    b.insert("start".into(), event_time(&start, tz.as_deref()));
    b.insert("end".into(), event_time(&end, tz.as_deref()));
    insert_nonempty(&mut b, "description", opt_string(a, "description")?);
    insert_nonempty(&mut b, "location", opt_string(a, "location")?);
    if let Some(v) = string_list(a, "recurrence")?.filter(|v| !v.is_empty()) {
        b.insert("recurrence".into(), json!(v));
    }
    if let Some(v) = attendees(a)? {
        let v = normalize_attendees(&v, None);
        if !v.is_empty() {
            b.insert("attendees".into(), json!(v));
        }
    }
    reminders(&mut b, a, None, true)?;
    choice(
        &mut b,
        "transparency",
        opt_string(a, "transparency")?,
        &["opaque", "transparent"],
    );
    choice(
        &mut b,
        "visibility",
        opt_string(a, "visibility")?,
        &["default", "public", "private", "confidential"],
    );
    optional_bool_field(&mut b, a, "guests_can_modify", "guestsCanModify")?;
    optional_bool_field(
        &mut b,
        a,
        "guests_can_invite_others",
        "guestsCanInviteOthers",
    )?;
    optional_bool_field(
        &mut b,
        a,
        "guests_can_see_other_guests",
        "guestsCanSeeOtherGuests",
    )?;
    if add_meet {
        b.insert("conferenceData".into(), json!({"createRequest":{"requestId":request_id(),"conferenceSolutionKey":{"type":"hangoutsMeet"}}}));
    } else if let Some(v) = conference.clone() {
        b.insert("conferenceData".into(), v);
    }
    let files = string_list(a, "attachments")?.unwrap_or_default();
    if !files.is_empty() {
        let mut out = Vec::new();
        for raw in files {
            if let Some(id) = drive_id(&raw) {
                let (title, mime) = drive_meta(s, &id).await.unwrap_or((
                    "Drive Attachment".into(),
                    "application/vnd.google-apps.drive-sdk".into(),
                ));
                out.push(json!({"fileUrl":format!("https://drive.google.com/open?id={id}"),"title":title,"mimeType":mime}));
            }
        }
        b.insert("attachments".into(), json!(out));
    }
    let mut q = vec![
        (
            "conferenceDataVersion".into(),
            i32::from(add_meet || conference.is_some()).to_string(),
        ),
        ("sendUpdates".into(), updates.into()),
    ];
    if b.contains_key("attachments") {
        q.push(("supportsAttachments".into(), "true".into()));
    }
    let r = send(
        s,
        "manage_event",
        Method::POST,
        &events_url(&cal),
        &q,
        Some(&Value::Object(b)),
    )
    .await?;
    let mut out = format!(
        "Successfully created event '{}' for {}. Link: {}",
        text(&r, "summary").unwrap_or(&summary),
        s.user_email,
        text(&r, "htmlLink").unwrap_or("No link available")
    );
    if (add_meet || conference.is_some())
        && let Some(link) = meeting_link(&r)
    {
        out.push_str(&format!(
            " {}: {link}",
            if add_meet {
                "Google Meet"
            } else {
                "Conference"
            }
        ));
    }
    Ok(out)
}

async fn update_event(
    s: &GoogleSession,
    a: &Map<String, Value>,
    updates: &str,
) -> Result<String, String> {
    let id = action_value(a, "event_id", "update")?;
    let cal = string_or(a, "calendar_id", "primary")?;
    let tz = opt_string(a, "timezone")?;
    let add_meet = opt_bool(a, "add_google_meet")?;
    let conf = conference(a, add_meet.unwrap_or(false))?;
    let summary = opt_string(a, "summary")?;
    let description = opt_string(a, "description")?;
    let location = opt_string(a, "location")?;
    let color = opt_string(a, "color_id")?;
    let recurrence = string_list(a, "recurrence")?;
    let atts = attendees(a)?;
    let mut b = Map::new();
    insert_some(&mut b, "summary", summary.clone());
    insert_some(&mut b, "description", description.clone());
    insert_some(&mut b, "location", location.clone());
    insert_some(&mut b, "colorId", color.clone());
    if let Some(v) = opt_string(a, "start_time")? {
        b.insert("start".into(), event_time(&v, tz.as_deref()));
    }
    if let Some(v) = opt_string(a, "end_time")? {
        b.insert("end".into(), event_time(&v, tz.as_deref()));
    }
    if let Some(v) = recurrence.as_ref() {
        b.insert("recurrence".into(), json!(v));
    }
    if let Some(v) = atts.as_ref() {
        let v = normalize_attendees(v, Some(&s.user_email));
        if !v.is_empty() {
            b.insert("attendees".into(), json!(v));
        }
    }
    choice(
        &mut b,
        "transparency",
        opt_string(a, "transparency")?,
        &["opaque", "transparent"],
    );
    choice(
        &mut b,
        "visibility",
        opt_string(a, "visibility")?,
        &["default", "public", "private", "confidential"],
    );
    optional_bool_field(&mut b, a, "guests_can_modify", "guestsCanModify")?;
    optional_bool_field(
        &mut b,
        a,
        "guests_can_invite_others",
        "guestsCanInviteOthers",
    )?;
    optional_bool_field(
        &mut b,
        a,
        "guests_can_see_other_guests",
        "guestsCanSeeOtherGuests",
    )?;
    if let Some(v) = conf.clone() {
        b.insert("conferenceData".into(), v);
    } else if let Some(v) = add_meet {
        b.insert("conferenceData".into(), if v { json!({"createRequest":{"requestId":request_id(),"conferenceSolutionKey":{"type":"hangoutsMeet"}}}) } else { Value::Null });
    }
    let reminder_change =
        arg(a, "reminders").is_some() || arg(a, "use_default_reminders").is_some();
    if b.is_empty() && !reminder_change {
        return Err("No fields provided to modify the event.".into());
    }
    let existing = match send(
        s,
        "manage_event",
        Method::GET,
        &event_url(&cal, &id),
        &[],
        None,
    )
    .await
    {
        Ok(value) => value,
        Err(error) if error.contains("404") => {
            return Err(format!(
                "Event not found during verification. The event with ID '{id}' could not be found in calendar '{cal}'. This may be due to incorrect ID format or the event no longer exists."
            ));
        }
        Err(_) => Value::Null,
    };
    reminders(&mut b, a, Some(&existing), false)?;
    preserve(&mut b, &existing, "summary", summary.is_some());
    preserve(&mut b, &existing, "description", description.is_some());
    preserve(&mut b, &existing, "location", location.is_some());
    preserve(&mut b, &existing, "colorId", color.is_some());
    preserve(&mut b, &existing, "recurrence", recurrence.is_some());
    preserve(&mut b, &existing, "attendees", atts.is_some());
    let r = send(
        s,
        "manage_event",
        Method::PATCH,
        &event_url(&cal, &id),
        &[
            ("conferenceDataVersion".into(), "1".into()),
            ("sendUpdates".into(), updates.into()),
        ],
        Some(&Value::Object(b)),
    )
    .await?;
    let mut out = format!(
        "Successfully modified event '{}' (ID: {id}) for {}. Link: {}",
        text(&r, "summary").or(summary.as_deref()).unwrap_or("None"),
        s.user_email,
        text(&r, "htmlLink").unwrap_or("No link available")
    );
    if conf.is_some() {
        if let Some(v) = meeting_link(&r) {
            out.push_str(&format!(" Conference: {v}"));
        }
    } else if add_meet == Some(true) {
        if let Some(v) = meeting_link(&r) {
            out.push_str(&format!(" Google Meet: {v}"));
        }
    } else if add_meet == Some(false) {
        out.push_str(" (Google Meet removed)");
    }
    Ok(out)
}

async fn delete_event(
    s: &GoogleSession,
    a: &Map<String, Value>,
    updates: &str,
) -> Result<String, String> {
    let id = action_value(a, "event_id", "delete")?;
    let cal = string_or(a, "calendar_id", "primary")?;
    if let Err(error) = send(
        s,
        "manage_event",
        Method::GET,
        &event_url(&cal, &id),
        &[],
        None,
    )
    .await
        && error.contains("404")
    {
        return Err(format!(
            "Event not found during verification. The event with ID '{id}' could not be found in calendar '{cal}'. This may be due to incorrect ID format or the event no longer exists."
        ));
    }
    send(
        s,
        "manage_event",
        Method::DELETE,
        &event_url(&cal, &id),
        &[("sendUpdates".into(), updates.into())],
        None,
    )
    .await?;
    Ok(format!(
        "Successfully deleted event (ID: {id}) from calendar '{cal}' for {}.",
        s.user_email
    ))
}

async fn rsvp_event(
    s: &GoogleSession,
    a: &Map<String, Value>,
    updates: &str,
) -> Result<String, String> {
    let id = action_value(a, "event_id", "rsvp")?;
    let response = action_value(a, "response", "rsvp")?;
    if !matches!(
        response.as_str(),
        "accepted" | "declined" | "tentative" | "needsAction"
    ) {
        return Err(format!(
            "Invalid response '{response}'. Must be one of: ['accepted', 'declined', 'needsAction', 'tentative']"
        ));
    }
    let cal = string_or(a, "calendar_id", "primary")?;
    let existing = send(
        s,
        "manage_event",
        Method::GET,
        &event_url(&cal, &id),
        &[],
        None,
    )
    .await?;
    if existing
        .pointer("/organizer/self")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err("You are the organizer of this event. Organizers cannot respond to their own invitations.".into());
    }
    let Some(source) = existing.get("attendees").and_then(Value::as_array) else {
        return Err("This event has no attendee list; cannot update RSVP.".into());
    };
    let mut all = source.clone();
    let Some(me) = all
        .iter_mut()
        .find(|v| v.get("self").and_then(Value::as_bool).unwrap_or(false))
    else {
        return Err(format!(
            "{} was not found in the event's attendee list.",
            s.user_email
        ));
    };
    let Some(me) = me.as_object_mut() else {
        return Err("Invalid attendee data returned by Google Calendar".into());
    };
    me.insert("responseStatus".into(), json!(response));
    if let Some(v) = opt_string(a, "rsvp_comment")? {
        me.insert("comment".into(), json!(v));
    }
    let r = send(
        s,
        "manage_event",
        Method::PATCH,
        &event_url(&cal, &id),
        &[("sendUpdates".into(), updates.into())],
        Some(&json!({"attendees":all})),
    )
    .await?;
    Ok(format!(
        "Successfully updated RSVP for '{}' (ID: {id}) to '{response}' for {}.",
        text(&r, "summary").unwrap_or("Unknown event"),
        s.user_email
    ))
}

#[derive(Clone, Copy)]
enum Special {
    Ooo,
    Focus,
}
impl Special {
    fn ty(self) -> &'static str {
        match self {
            Self::Ooo => "outOfOffice",
            Self::Focus => "focusTime",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Ooo => "Out of Office",
            Self::Focus => "Focus Time",
        }
    }
    fn prop(self) -> &'static str {
        match self {
            Self::Ooo => "outOfOfficeProperties",
            Self::Focus => "focusTimeProperties",
        }
    }
    fn tool(self) -> &'static str {
        match self {
            Self::Ooo => "manage_out_of_office",
            Self::Focus => "manage_focus_time",
        }
    }
}

async fn manage_special(a: &Map<String, Value>, kind: Special) -> Result<String, String> {
    validate(
        a,
        if matches!(kind, Special::Ooo) {
            OOO_ARGS
        } else {
            FOCUS_ARGS
        },
    )?;
    let action = required(a, "action")?.trim().to_lowercase();
    let s = auth(a, EVENTS).await?;
    match action.as_str() {
        "create" => special_create(&s, a, kind).await,
        "list" => special_list(&s, a, kind).await,
        "update" => special_update(&s, a, kind).await,
        "delete" => special_delete(&s, a, kind).await,
        _ => Err(format!(
            "Invalid action '{action}'. Must be 'create', 'list', 'update', or 'delete'."
        )),
    }
}

async fn special_create(
    s: &GoogleSession,
    a: &Map<String, Value>,
    kind: Special,
) -> Result<String, String> {
    let start = opt_string(a, "start_time")?.filter(|v| !v.is_empty());
    let end = opt_string(a, "end_time")?.filter(|v| !v.is_empty());
    if start.is_none() || end.is_none() {
        return Err("start_time and end_time are required for create action".into());
    }
    let start = start.unwrap_or_default();
    let end = end.unwrap_or_default();
    let cal = string_or(a, "calendar_id", "primary")?;
    let tz = opt_string(a, "timezone")?;
    let summary = opt_string(a, "summary")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| kind.name().into());
    let mode = decline_mode(
        opt_string(a, "auto_decline_mode")?.as_deref(),
        if matches!(kind, Special::Ooo) {
            "create_ooo_event"
        } else {
            "create_focus_time_event"
        },
    )?;
    let message = opt_string(a, "decline_message")?.unwrap_or_default();
    let mut props = Map::new();
    props.insert("autoDeclineMode".into(), json!(mode));
    props.insert("declineMessage".into(), json!(message));
    let chat = if matches!(kind, Special::Focus) {
        let v = opt_string(a, "chat_status")?.unwrap_or_else(|| "doNotDisturb".into());
        chat_status(&v, "create_focus_time_event")?;
        props.insert("chatStatus".into(), json!(v));
        Some(v)
    } else {
        None
    };
    let mut b = Map::new();
    b.insert("eventType".into(), json!(kind.ty()));
    b.insert("summary".into(), json!(summary));
    b.insert("start".into(), special_time(&start, tz.as_deref(), kind)?);
    b.insert("end".into(), special_time(&end, tz.as_deref(), kind)?);
    b.insert(kind.prop().into(), Value::Object(props));
    b.insert("transparency".into(), json!("opaque"));
    if matches!(kind, Special::Focus) {
        insert_nonempty(&mut b, "description", opt_string(a, "description")?)
    }
    if let Some(v) = string_list(a, "recurrence")?.filter(|v| !v.is_empty()) {
        b.insert("recurrence".into(), json!(v));
    }
    let r = send(
        s,
        kind.tool(),
        Method::POST,
        &events_url(&cal),
        &[],
        Some(&Value::Object(b)),
    )
    .await?;
    let mut out = format!(
        "Successfully created {} event for {}.\n- Summary: {summary}\n- Start: {}\n- End: {}\n- Auto-decline: {mode}\n- Decline message: {}\n",
        kind.name(),
        s.user_email,
        raw_time(&r, "start"),
        raw_time(&r, "end"),
        if message.is_empty() {
            "(none)"
        } else {
            &message
        }
    );
    if let Some(v) = chat {
        out.push_str(&format!("- Chat status: {v}\n"));
    }
    out.push_str(&format!(
        "- Event ID: {}\n- Link: {}",
        text(&r, "id").unwrap_or("N/A"),
        text(&r, "htmlLink").unwrap_or("N/A")
    ));
    Ok(out)
}

async fn special_list(
    s: &GoogleSession,
    a: &Map<String, Value>,
    kind: Special,
) -> Result<String, String> {
    let cal = string_or(a, "calendar_id", "primary")?;
    let timezone = opt_string(a, "timezone")?;
    let mut q = vec![
        (
            "timeMin".into(),
            normalize_time_in_zone(opt_string(a, "time_min")?.as_deref(), timezone.as_deref())?
                .unwrap_or_else(now),
        ),
        (
            "maxResults".into(),
            int_or(a, "max_results", 10)?.to_string(),
        ),
        ("singleEvents".into(), "true".into()),
        ("orderBy".into(), "startTime".into()),
        ("eventTypes".into(), kind.ty().into()),
    ];
    push(
        &mut q,
        "timeMax",
        normalize_time_in_zone(opt_string(a, "time_max")?.as_deref(), timezone.as_deref())?
            .as_deref(),
    );
    let r = send(s, kind.tool(), Method::GET, &events_url(&cal), &q, None).await?;
    let items = arr(&r, "items");
    if items.is_empty() {
        return Ok(if matches!(kind, Special::Ooo) {
            format!("No out-of-office events found for {}.", s.user_email)
        } else {
            format!("No Focus Time events found for {}.", s.user_email)
        });
    }
    let label = if matches!(kind, Special::Ooo) {
        "out-of-office"
    } else {
        "Focus Time"
    };
    let mut lines = vec![format!(
        "Found {} {label} event(s) for {}:\n",
        items.len(),
        s.user_email
    )];
    for (i, e) in items.iter().enumerate() {
        let p = e.get(kind.prop()).and_then(Value::as_object);
        lines.push(format!(
            "{}. \"{}\" ({} to {})",
            i + 1,
            text(e, "summary").unwrap_or(kind.name()),
            raw_time(e, "start"),
            raw_time(e, "end")
        ));
        lines.push(format!(
            "   Auto-decline: {}",
            p.and_then(|v| v.get("autoDeclineMode"))
                .and_then(Value::as_str)
                .unwrap_or("N/A")
        ));
        if let Some(v) = p
            .and_then(|v| v.get("declineMessage"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            lines.push(format!("   Decline message: {v}"));
        }
        if matches!(kind, Special::Focus)
            && let Some(v) = p
                .and_then(|v| v.get("chatStatus"))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        {
            lines.push(format!("   Chat status: {v}"));
        }
        lines.push(format!("   Event ID: {}", text(e, "id").unwrap_or("N/A")));
        lines.push(String::new());
    }
    Ok(lines.join("\n").trim_end().into())
}

async fn special_update(
    s: &GoogleSession,
    a: &Map<String, Value>,
    kind: Special,
) -> Result<String, String> {
    let id = action_value(a, "event_id", "update")?;
    let cal = string_or(a, "calendar_id", "primary")?;
    let old = send(
        s,
        kind.tool(),
        Method::GET,
        &event_url(&cal, &id),
        &[],
        None,
    )
    .await?;
    check_type(&old, &id, kind, "update")?;
    let tz = opt_string(a, "timezone")?;
    let mut b = Map::new();
    insert_some(&mut b, "summary", opt_string(a, "summary")?);
    if matches!(kind, Special::Focus) {
        insert_some(&mut b, "description", opt_string(a, "description")?)
    }
    if let Some(v) = opt_string(a, "start_time")? {
        b.insert("start".into(), special_time(&v, tz.as_deref(), kind)?);
    }
    if let Some(v) = opt_string(a, "end_time")? {
        b.insert("end".into(), special_time(&v, tz.as_deref(), kind)?);
    }
    if let Some(v) = string_list(a, "recurrence")? {
        b.insert("recurrence".into(), json!(v));
    }
    let mode = opt_string(a, "auto_decline_mode")?;
    let msg = opt_string(a, "decline_message")?;
    let chat = if matches!(kind, Special::Focus) {
        opt_string(a, "chat_status")?
    } else {
        None
    };
    if mode.is_some() || msg.is_some() || chat.is_some() {
        let p = old.get(kind.prop()).and_then(Value::as_object);
        let mode = if let Some(v) = mode {
            decline_mode(
                Some(&v),
                if matches!(kind, Special::Ooo) {
                    "update_ooo_event"
                } else {
                    "update_focus_time_event"
                },
            )?
        } else {
            p.and_then(|v| v.get("autoDeclineMode"))
                .and_then(Value::as_str)
                .unwrap_or("declineAllConflictingInvitations")
                .into()
        };
        let msg = msg.unwrap_or_else(|| {
            p.and_then(|v| v.get("declineMessage"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .into()
        });
        let mut np = Map::new();
        np.insert("autoDeclineMode".into(), json!(mode));
        np.insert("declineMessage".into(), json!(msg));
        if matches!(kind, Special::Focus) {
            if let Some(v) = chat {
                chat_status(&v, "update_focus_time_event")?;
                np.insert("chatStatus".into(), json!(v));
            } else if let Some(v) = p.and_then(|v| v.get("chatStatus")).and_then(Value::as_str) {
                np.insert("chatStatus".into(), json!(v));
            }
        }
        b.insert(kind.prop().into(), Value::Object(np));
    }
    if b.is_empty() {
        return Ok(format!(
            "No changes specified for {} event '{id}'.",
            kind.name()
        ));
    }
    let r = send(
        s,
        kind.tool(),
        Method::PATCH,
        &event_url(&cal, &id),
        &[],
        Some(&Value::Object(b)),
    )
    .await?;
    Ok(format!(
        "Successfully updated {} event (ID: {id}) for {}.\n- Summary: {}\n- Start: {}\n- End: {}\n- Link: {}",
        kind.name(),
        s.user_email,
        text(&r, "summary").unwrap_or(kind.name()),
        raw_time(&r, "start"),
        raw_time(&r, "end"),
        text(&r, "htmlLink").unwrap_or("N/A")
    ))
}
async fn special_delete(
    s: &GoogleSession,
    a: &Map<String, Value>,
    kind: Special,
) -> Result<String, String> {
    let id = action_value(a, "event_id", "delete")?;
    let cal = string_or(a, "calendar_id", "primary")?;
    let old = send(
        s,
        kind.tool(),
        Method::GET,
        &event_url(&cal, &id),
        &[],
        None,
    )
    .await
    .map_err(|e| {
        if e.contains("404") {
            format!(
                "Event not found. The event with ID '{id}' could not be found in calendar '{cal}'."
            )
        } else {
            e
        }
    })?;
    check_type(&old, &id, kind, "delete")?;
    send(
        s,
        kind.tool(),
        Method::DELETE,
        &event_url(&cal, &id),
        &[],
        None,
    )
    .await?;
    Ok(format!(
        "Successfully deleted {} event (ID: {id}) from calendar '{cal}' for {}.",
        kind.name(),
        s.user_email
    ))
}

async fn query_freebusy(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, FREEBUSY_ARGS)?;
    let min = normalize_time(Some(&required(a, "time_min")?))?.ok_or("time_min is required")?;
    let max = normalize_time(Some(&required(a, "time_max")?))?.ok_or("time_max is required")?;
    let ids = string_list(a, "calendar_ids")?
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec!["primary".into()]);
    let mut b = Map::new();
    b.insert("timeMin".into(), json!(min));
    b.insert("timeMax".into(), json!(max));
    b.insert(
        "items".into(),
        json!(
            ids.into_iter()
                .map(|id| json!({"id":id}))
                .collect::<Vec<_>>()
        ),
    );
    if let Some(v) = opt_int(a, "group_expansion_max")? {
        b.insert("groupExpansionMax".into(), json!(v));
    }
    if let Some(v) = opt_int(a, "calendar_expansion_max")? {
        b.insert("calendarExpansionMax".into(), json!(v));
    }
    let s = auth(a, READ).await?;
    let r = send(
        &s,
        "query_freebusy",
        Method::POST,
        &format!("{API}/freeBusy"),
        &[],
        Some(&Value::Object(b)),
    )
    .await?;
    let Some(cals) = r
        .get("calendars")
        .and_then(Value::as_object)
        .filter(|v| !v.is_empty())
    else {
        return Ok(format!(
            "No free/busy information found for the requested calendars for {}.",
            s.user_email
        ));
    };
    let mut lines = vec![
        format!("Free/Busy information for {}:", s.user_email),
        format!(
            "Time range: {} to {}",
            text(&r, "timeMin").unwrap_or(&min),
            text(&r, "timeMax").unwrap_or(&max)
        ),
        String::new(),
    ];
    for (id, data) in cals {
        lines.push(format!("Calendar: {id}"));
        let errors = arr(data, "errors");
        if !errors.is_empty() {
            lines.push("  Errors:".into());
            for e in errors {
                lines.push(format!(
                    "    - {}: {}",
                    text(e, "domain").unwrap_or("unknown"),
                    text(e, "reason").unwrap_or("unknown")
                ));
            }
            lines.push(String::new());
            continue;
        }
        let busy = arr(data, "busy");
        if busy.is_empty() {
            lines.push("  Status: Free (no busy periods)".into())
        } else {
            lines.push(format!("  Busy periods: {}", busy.len()));
            for p in busy {
                lines.push(format!(
                    "    - {} to {}",
                    text(p, "start").unwrap_or("Unknown"),
                    text(p, "end").unwrap_or("Unknown")
                ));
            }
        }
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}
async fn create_calendar(a: &Map<String, Value>) -> Result<String, String> {
    validate(a, CREATE_CALENDAR_ARGS)?;
    let summary = required(a, "summary")?;
    let mut b = Map::new();
    b.insert("summary".into(), json!(summary));
    insert_nonempty(&mut b, "description", opt_string(a, "description")?);
    insert_nonempty(&mut b, "timeZone", opt_string(a, "timezone")?);
    let s = auth(a, CALENDAR).await?;
    let r = send(
        &s,
        "create_calendar",
        Method::POST,
        &format!("{API}/calendars"),
        &[],
        Some(&Value::Object(b)),
    )
    .await?;
    Ok(format!(
        "Created calendar '{}' (ID: {})",
        text(&r, "summary").unwrap_or(&summary),
        required_api(&r, "id")?
    ))
}

async fn auth(a: &Map<String, Value>, scope: &str) -> Result<GoogleSession, String> {
    let supplied = required(a, "user_google_email")?;
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
    let mut r = s.request(method, url);
    if !q.is_empty() {
        r = r.query(q)
    }
    if let Some(v) = body {
        r = r.json(v)
    }
    s.send_json(tool, r).await
}
fn events_url(cal: &str) -> String {
    format!("{API}/calendars/{}/events", segment(cal))
}
fn event_url(cal: &str, id: &str) -> String {
    format!("{}/{}", events_url(cal), segment(id))
}
fn segment(v: &str) -> String {
    utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()
}

fn validate(a: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    for (key, value) in a {
        let Some(name) = allowed
            .iter()
            .find(|name| key == **name || key == &camel(name))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        let camel_name = camel(name);
        if camel_name != **name && key == &camel_name && a.contains_key(*name) {
            return Err(format!("Unexpected argument: {key}"));
        }
        if value.is_null() {
            if matches!(
                *name,
                "user_google_email"
                    | "action"
                    | "calendar_id"
                    | "max_results"
                    | "detailed"
                    | "include_attachments"
            ) {
                let expected = if matches!(*name, "max_results") {
                    "an integer"
                } else if matches!(*name, "detailed" | "include_attachments") {
                    "a boolean"
                } else {
                    "a string"
                };
                return Err(format!("{name} must be {expected}"));
            }
            continue;
        }
        if matches!(
            *name,
            "max_results" | "group_expansion_max" | "calendar_expansion_max"
        ) && !value.is_i64()
        {
            return Err(format!("{name} must be an integer"));
        } else if matches!(
            *name,
            "detailed"
                | "include_attachments"
                | "add_google_meet"
                | "use_default_reminders"
                | "guests_can_modify"
                | "guests_can_invite_others"
                | "guests_can_see_other_guests"
        ) && !value.is_boolean()
        {
            return Err(format!("{name} must be a boolean"));
        } else if matches!(*name, "attachments" | "recurrence" | "calendar_ids") {
            parse_string_list(value, name).map(|_| ())?;
        } else if *name == "attendees" {
            parse_attendees(value)?;
        } else if *name == "reminders"
            && !value.is_string()
            && !value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object))
        {
            return Err("reminders must be a JSON string or an array of objects".into());
        } else if *name == "conference_data" && !value.is_object() {
            return Err("conference_data must be an object".into());
        } else if !matches!(
            *name,
            "max_results"
                | "group_expansion_max"
                | "calendar_expansion_max"
                | "detailed"
                | "include_attachments"
                | "add_google_meet"
                | "use_default_reminders"
                | "guests_can_modify"
                | "guests_can_invite_others"
                | "guests_can_see_other_guests"
                | "attachments"
                | "recurrence"
                | "calendar_ids"
                | "attendees"
                | "reminders"
                | "conference_data"
        ) && !value.is_string()
        {
            return Err(format!("{name} must be a string"));
        }
    }
    Ok(())
}
fn camel(v: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in v.chars() {
        if c == '_' {
            up = true
        } else if up {
            out.extend(c.to_uppercase());
            up = false
        } else {
            out.push(c)
        }
    }
    out
}
fn arg<'a>(a: &'a Map<String, Value>, n: &str) -> Option<&'a Value> {
    a.get(n).or_else(|| a.get(&camel(n)))
}
fn opt_string(a: &Map<String, Value>, n: &str) -> Result<Option<String>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|x| Some(x.into()))
            .ok_or_else(|| format!("{n} must be a string")),
    }
}
fn required(a: &Map<String, Value>, n: &str) -> Result<String, String> {
    opt_string(a, n)?.ok_or_else(|| format!("{n} is required"))
}
fn string_or(a: &Map<String, Value>, n: &str, d: &str) -> Result<String, String> {
    Ok(opt_string(a, n)?.unwrap_or_else(|| d.into()))
}
fn action_value(a: &Map<String, Value>, n: &str, action: &str) -> Result<String, String> {
    opt_string(a, n)?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{n} is required for {action} action"))
}
fn opt_int(a: &Map<String, Value>, n: &str) -> Result<Option<i64>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{n} must be an integer")),
    }
}
fn int_or(a: &Map<String, Value>, n: &str, d: i64) -> Result<i64, String> {
    Ok(opt_int(a, n)?.unwrap_or(d))
}
fn opt_bool(a: &Map<String, Value>, n: &str) -> Result<Option<bool>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{n} must be a boolean")),
    }
}
fn bool_or(a: &Map<String, Value>, n: &str, d: bool) -> Result<bool, String> {
    Ok(opt_bool(a, n)?.unwrap_or(d))
}
fn parse_string_list(v: &Value, n: &str) -> Result<Vec<String>, String> {
    let parsed;
    let v = if let Some(s) = v.as_str() {
        parsed = serde_json::from_str::<Value>(s)
            .map_err(|_| format!("{n} must be an array of strings or a JSON string array"))?;
        &parsed
    } else {
        v
    };
    v.as_array()
        .filter(|x| x.iter().all(Value::is_string))
        .map(|x| {
            x.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .ok_or_else(|| format!("{n} must be an array of strings or a JSON string array"))
}
fn string_list(a: &Map<String, Value>, n: &str) -> Result<Option<Vec<String>>, String> {
    match arg(a, n) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => parse_string_list(v, n).map(Some),
    }
}
fn parse_attendees(v: &Value) -> Result<(), String> {
    let parsed;
    let v = if let Some(s) = v.as_str() {
        parsed = serde_json::from_str::<Value>(s)
            .map_err(|_| "attendees must be an array of strings or objects")?;
        &parsed
    } else {
        v
    };
    if v.as_array()
        .is_some_and(|x| x.iter().all(|y| y.is_string() || y.is_object()))
    {
        Ok(())
    } else {
        Err("attendees must be an array of strings or objects".into())
    }
}
fn attendees(a: &Map<String, Value>) -> Result<Option<Vec<Value>>, String> {
    match arg(a, "attendees") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let parsed;
            let v = if let Some(s) = v.as_str() {
                parsed = serde_json::from_str(s)
                    .map_err(|_| "attendees must be an array of strings or objects")?;
                &parsed
            } else {
                v
            };
            v.as_array()
                .cloned()
                .map(Some)
                .ok_or_else(|| "attendees must be an array of strings or objects".into())
        }
    }
}

fn event_time(v: &str, tz: Option<&str>) -> Value {
    if !v.contains('T') {
        return json!({"date":v});
    }
    let mut m = Map::new();
    m.insert(
        "dateTime".into(),
        json!(if tz.is_some() {
            strip_offset(v)
        } else {
            v.into()
        }),
    );
    if let Some(z) = tz {
        m.insert("timeZone".into(), json!(z));
    }
    Value::Object(m)
}
fn special_time(v: &str, tz: Option<&str>, kind: Special) -> Result<Value, String> {
    let dt = if v.contains('T') {
        v.into()
    } else {
        format!("{v}T00:00:00")
    };
    if tz.is_none() && !explicit_offset(&dt) {
        return Err(format!(
            "{} events require either a timezone parameter or a start/end timestamp with an explicit UTC offset.",
            kind.name()
        ));
    }
    let mut m = Map::new();
    m.insert("dateTime".into(), json!(dt));
    if let Some(z) = tz {
        m.insert("timeZone".into(), json!(z));
    }
    Ok(Value::Object(m))
}
fn strip_offset(v: &str) -> String {
    if let Some(x) = v.strip_suffix('Z') {
        return x.into();
    }
    if v.len() >= 6 {
        let s = &v[v.len() - 6..];
        let b = s.as_bytes();
        if matches!(b[0], b'+' | b'-')
            && b[1..3].iter().all(u8::is_ascii_digit)
            && b[3] == b':'
            && b[4..].iter().all(u8::is_ascii_digit)
        {
            return v[..v.len() - 6].into();
        }
    }
    v.into()
}
fn explicit_offset(v: &str) -> bool {
    v.ends_with('Z') || strip_offset(v) != v
}
fn parse_date(v: &str) -> Option<Date> {
    if v.len() != 10 {
        return None;
    }
    let y = v.get(..4)?.parse().ok()?;
    let m = Month::try_from(v.get(5..7)?.parse::<u8>().ok()?).ok()?;
    let d = v.get(8..)?.parse().ok()?;
    Date::from_calendar_date(y, m, d).ok()
}
fn normalize_time(v: Option<&str>) -> Result<Option<String>, String> {
    let Some(v) = v else { return Ok(None) };
    let v = v.trim().trim_matches(|c| c == '\"' || c == '\'').trim();
    if v.is_empty() || matches!(v.to_ascii_lowercase().as_str(), "null" | "none") {
        return Ok(None);
    }
    if v.len() == 10 && parse_date(v).is_some() {
        return Ok(Some(format!("{v}T00:00:00Z")));
    }
    if v.len() == 19 && v.as_bytes().get(10) == Some(&b'T') {
        let valid = parse_date(&v[..10]).is_some()
            && v[11..13].parse::<u8>().is_ok_and(|x| x < 24)
            && v[14..16].parse::<u8>().is_ok_and(|x| x < 60)
            && v[17..19].parse::<u8>().is_ok_and(|x| x < 60)
            && v.as_bytes()[13] == b':'
            && v.as_bytes()[16] == b':';
        if valid {
            return Ok(Some(format!("{v}Z")));
        }
    }
    Ok(Some(v.into()))
}

fn normalize_time_in_zone(
    value: Option<&str>,
    timezone: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw
        .trim()
        .trim_matches(|character| character == '"' || character == '\'')
        .trim();
    let Some(date) = parse_date(trimmed) else {
        return normalize_time(value);
    };
    let Some(timezone) = timezone.filter(|value| !value.is_empty()) else {
        return normalize_time(value);
    };
    let local_midnight = date
        .with_hms(0, 0, 0)
        .expect("midnight is valid")
        .assume_utc()
        .unix_timestamp();
    let offset = zone_offset_for_local_midnight(timezone, local_midnight).unwrap_or(0);
    let utc = OffsetDateTime::from_unix_timestamp(local_midnight - i64::from(offset))
        .map_err(|_| String::from("time is outside the supported RFC3339 range"))?;
    Ok(Some(
        utc.format(&Rfc3339)
            .unwrap_or_else(|_| format!("{trimmed}T00:00:00Z")),
    ))
}

fn zone_offset_for_local_midnight(timezone: &str, local_timestamp: i64) -> Option<i32> {
    if timezone.starts_with('/')
        || timezone
            .split('/')
            .any(|part| part.is_empty() || part == "..")
    {
        return None;
    }
    let root = Path::new("/usr/share/zoneinfo");
    let bytes = fs::read(root.join(timezone)).ok()?;
    let (transitions, indexes, offsets) = parse_tzif(&bytes)?;
    let offset_at = |timestamp: i64| {
        let position = transitions.partition_point(|transition| *transition <= timestamp);
        let index = if position == 0 {
            0
        } else {
            usize::from(*indexes.get(position - 1)?)
        };
        offsets.get(index).copied()
    };
    let first = offset_at(local_timestamp)?;
    offset_at(local_timestamp - i64::from(first)).or(Some(first))
}

fn parse_tzif(bytes: &[u8]) -> Option<(Vec<i64>, Vec<u8>, Vec<i32>)> {
    fn u32_be(bytes: &[u8], index: usize) -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(index..index + 4)?.try_into().ok()?,
        ))
    }
    fn counts(bytes: &[u8], header: usize) -> Option<[usize; 6]> {
        let mut result = [0; 6];
        for (index, value) in result.iter_mut().enumerate() {
            *value = usize::try_from(u32_be(bytes, header + 20 + index * 4)?).ok()?;
        }
        Some(result)
    }
    fn block_size(counts: [usize; 6], width: usize) -> Option<usize> {
        let [gmt, standard, leap, time, types, chars] = counts;
        let mut size = time.checked_mul(width)?.checked_add(time)?;
        size = size.checked_add(types.checked_mul(6)?)?;
        size = size.checked_add(chars)?;
        size = size.checked_add(leap.checked_mul(width + 4)?)?;
        size = size.checked_add(standard)?.checked_add(gmt)?;
        Some(size)
    }
    if bytes.get(..4)? != b"TZif" {
        return None;
    }
    let version = *bytes.get(4)?;
    let (header, width) = if matches!(version, b'2' | b'3' | b'4') {
        let first = counts(bytes, 0)?;
        (44usize.checked_add(block_size(first, 4)?)?, 8)
    } else {
        (0, 4)
    };
    if bytes.get(header..header + 4)? != b"TZif" {
        return None;
    }
    let c = counts(bytes, header)?;
    let time_count = c[3];
    let type_count = c[4];
    let mut cursor = header + 44;
    let mut transitions = Vec::with_capacity(time_count);
    for _ in 0..time_count {
        let timestamp = if width == 8 {
            i64::from_be_bytes(bytes.get(cursor..cursor + 8)?.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(
                bytes.get(cursor..cursor + 4)?.try_into().ok()?,
            ))
        };
        transitions.push(timestamp);
        cursor += width;
    }
    let indexes = bytes.get(cursor..cursor + time_count)?.to_vec();
    cursor += time_count;
    let mut offsets = Vec::with_capacity(type_count);
    for _ in 0..type_count {
        offsets.push(i32::from_be_bytes(
            bytes.get(cursor..cursor + 4)?.try_into().ok()?,
        ));
        cursor += 6;
    }
    Some((transitions, indexes, offsets))
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}
fn insert_nonempty(b: &mut Map<String, Value>, n: &str, v: Option<String>) {
    if let Some(v) = v.filter(|x| !x.is_empty()) {
        b.insert(n.into(), json!(v));
    }
}
fn insert_some(b: &mut Map<String, Value>, n: &str, v: Option<String>) {
    if let Some(v) = v {
        b.insert(n.into(), json!(v));
    }
}
fn optional_bool_field(
    b: &mut Map<String, Value>,
    a: &Map<String, Value>,
    n: &str,
    api: &str,
) -> Result<(), String> {
    if let Some(v) = opt_bool(a, n)? {
        b.insert(api.into(), json!(v));
    }
    Ok(())
}
fn choice(b: &mut Map<String, Value>, n: &str, v: Option<String>, valid: &[&str]) {
    if let Some(v) = v.filter(|x| valid.contains(&x.as_str())) {
        b.insert(n.into(), json!(v));
    }
}
fn push(q: &mut Vec<(String, String)>, n: &str, v: Option<&str>) {
    if let Some(v) = v.filter(|x| !x.is_empty()) {
        q.push((n.into(), v.into()));
    }
}
fn preserve(b: &mut Map<String, Value>, old: &Value, n: &str, provided: bool) {
    if !provided && let Some(v) = old.get(n) {
        b.insert(n.into(), v.clone());
    }
}
fn request_id() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_nanos())
        .unwrap_or_default();
    format!(
        "workspace-mcp-{n:x}-{:x}",
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}
fn conference(a: &Map<String, Value>, meet: bool) -> Result<Option<Value>, String> {
    let raw = arg(a, "conference_data").filter(|v| !v.is_null()).cloned();
    let provider = opt_string(a, "conference_provider")?;
    let uri = opt_string(a, "conference_uri")?;
    let pass = opt_string(a, "conference_passcode")?;
    let cid = opt_string(a, "conference_id")?;
    let helper = [provider.as_ref(), uri.as_ref(), pass.as_ref(), cid.as_ref()]
        .iter()
        .any(|v| v.is_some_and(|x| !x.is_empty()));
    if raw.is_some() && helper {
        return Err("Provide either conference_data (raw payload) or the conference_provider/conference_uri helper params, not both.".into());
    }
    let resolved = if helper {
        let p = provider.unwrap_or_default().trim().to_owned();
        let u = uri.unwrap_or_default().trim().to_owned();
        if p.is_empty() || u.is_empty() {
            return Err("conference_provider and conference_uri are both required to attach a third-party conference.".into());
        }
        let name = match p.to_lowercase().as_str() {
            "zoom" => "Zoom Meeting",
            "webex" => "Webex",
            "teams" | "microsoft teams" => "Microsoft Teams",
            _ => &p,
        };
        let mut ep = json!({"entryPointType":"video","uri":u,"label":name});
        if let Some(v) = pass.filter(|v| !v.is_empty()) {
            ep.as_object_mut()
                .unwrap()
                .insert("passcode".into(), json!(v));
        }
        let mut d =
            json!({"conferenceSolution":{"key":{"type":"addOn"},"name":name},"entryPoints":[ep]});
        if let Some(v) = cid.filter(|v| !v.is_empty()) {
            d.as_object_mut()
                .unwrap()
                .insert("conferenceId".into(), json!(v));
        }
        Some(d)
    } else {
        raw
    };
    if resolved.is_some() && meet {
        return Err("Cannot attach a third-party conference and add_google_meet on the same event; choose one.".into());
    }
    Ok(resolved)
}
fn parse_reminders(v: Option<&Value>) -> Vec<Value> {
    let Some(v) = v.filter(|x| !x.is_null()) else {
        return vec![];
    };
    let parsed;
    let v = if let Some(s) = v.as_str() {
        let Ok(x) = serde_json::from_str(s) else {
            return vec![];
        };
        parsed = x;
        &parsed
    } else {
        v
    };
    let Some(v) = v.as_array() else { return vec![] };
    v.iter()
        .take(5)
        .filter_map(|x| {
            let x = x.as_object()?;
            let method = x.get("method")?.as_str()?.to_lowercase();
            if !matches!(method.as_str(), "popup" | "email") {
                return None;
            }
            let minutes = x.get("minutes")?.as_i64()?;
            if !(0..=40320).contains(&minutes) {
                return None;
            }
            Some(json!({"method":method,"minutes":minutes}))
        })
        .collect()
}
fn reminders(
    b: &mut Map<String, Value>,
    a: &Map<String, Value>,
    old: Option<&Value>,
    create: bool,
) -> Result<(), String> {
    let has = arg(a, "reminders").is_some_and(|v| !v.is_null());
    let has_default = arg(a, "use_default_reminders").is_some_and(|v| !v.is_null());
    let provided = opt_bool(a, "use_default_reminders")?;
    if (create && !has && provided != Some(false)) || (!create && !has && !has_default) {
        return Ok(());
    }
    let mut use_default = provided.unwrap_or_else(|| {
        old.and_then(|v| v.pointer("/reminders/useDefault"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    });
    if has {
        use_default = false
    }
    let mut m = Map::new();
    m.insert("useDefault".into(), json!(use_default));
    if has {
        let v = parse_reminders(arg(a, "reminders"));
        if !v.is_empty() {
            m.insert("overrides".into(), json!(v));
        }
    }
    b.insert("reminders".into(), Value::Object(m));
    Ok(())
}
fn normalize_attendees(v: &[Value], self_email: Option<&str>) -> Vec<Value> {
    const FIELDS: &[&str] = &[
        "email",
        "displayName",
        "optional",
        "resource",
        "responseStatus",
        "comment",
        "additionalGuests",
    ];
    v.iter()
        .filter_map(|x| {
            if let Some(e) = x.as_str() {
                return Some(json!({"email":e}));
            }
            let src = x.as_object()?;
            let email = src.get("email")?.as_str()?;
            let is_self = self_email.is_none_or(|s| email.trim().eq_ignore_ascii_case(s.trim()));
            let mut out = Map::new();
            for f in FIELDS {
                if !is_self && matches!(*f, "responseStatus" | "comment") {
                    continue;
                }
                if let Some(v) = src.get(*f) {
                    out.insert((*f).into(), v.clone());
                }
            }
            Some(Value::Object(out))
        })
        .collect()
}
fn decline_mode(v: Option<&str>, func: &str) -> Result<String, String> {
    let v = v.unwrap_or("declineAllConflictingInvitations");
    if !matches!(
        v,
        "declineAllConflictingInvitations" | "declineOnlyNewConflictingInvitations" | "declineNone"
    ) {
        return Err(format!(
            "[{func}] Invalid auto_decline_mode '{v}'. Must be one of: declineAllConflictingInvitations, declineNone, declineOnlyNewConflictingInvitations"
        ));
    }
    Ok(v.into())
}
fn chat_status(v: &str, func: &str) -> Result<(), String> {
    if !matches!(v, "available" | "doNotDisturb") {
        return Err(format!(
            "[{func}] Invalid chat_status '{v}'. Must be one of: available, doNotDisturb"
        ));
    }
    Ok(())
}
fn check_type(e: &Value, id: &str, k: Special, action: &str) -> Result<(), String> {
    let ty = text(e, "eventType").unwrap_or("default");
    if ty != k.ty() {
        return Err(format!(
            "Event '{id}' is not an {} event (type: '{ty}'). Use manage_event to {action} regular events.",
            k.name()
        ));
    }
    Ok(())
}
fn drive_id(v: &str) -> Option<String> {
    if !v.starts_with("https://") {
        return (!v.is_empty()).then(|| v.into());
    }
    for marker in ["/file/d/", "/d/", "id="] {
        if let Some(i) = v.find(marker) {
            let id: String = v[i + marker.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}
async fn drive_meta(s: &GoogleSession, id: &str) -> Option<(String, String)> {
    let r = send(
        s,
        "manage_event",
        Method::GET,
        &format!("{DRIVE_API}/files/{}", segment(id)),
        &[
            ("fields".into(), "mimeType,name".into()),
            ("supportsAllDrives".into(), "true".into()),
        ],
        None,
    )
    .await
    .ok()?;
    Some((
        text(&r, "name").unwrap_or("Drive Attachment").into(),
        text(&r, "mimeType")
            .unwrap_or("application/vnd.google-apps.drive-sdk")
            .into(),
    ))
}
fn meeting_link(e: &Value) -> Option<&str> {
    if let Some(points) = e
        .pointer("/conferenceData/entryPoints")
        .and_then(Value::as_array)
    {
        for p in points {
            if text(p, "entryPointType") == Some("video")
                && let Some(v) = text(p, "uri").filter(|v| !v.is_empty())
            {
                return Some(v);
            }
        }
    }
    text(e, "hangoutLink").filter(|v| !v.is_empty())
}
fn raw_time<'a>(e: &'a Value, n: &str) -> &'a str {
    e.get(n)
        .and_then(Value::as_object)
        .and_then(|v| v.get("date").or_else(|| v.get("dateTime")))
        .and_then(Value::as_str)
        .unwrap_or("N/A")
}
fn format_time(e: &Value, n: &str) -> String {
    let Some(b) = e.get(n).and_then(Value::as_object) else {
        return "None".into();
    };
    let (v, all) = if let Some(v) = b.get("dateTime").and_then(Value::as_str) {
        (v, false)
    } else if let Some(v) = b.get("date").and_then(Value::as_str) {
        (v, true)
    } else {
        return "None".into();
    };
    let date = if all {
        parse_date(v)
    } else {
        OffsetDateTime::parse(v, &Rfc3339).ok().map(|x| x.date())
    };
    let Some(date) = date else { return v.into() };
    let iso = date.weekday().number_from_monday();
    let day = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ][usize::from(iso - 1)];
    format!(
        "{v} [weekday: {day}; ISO weekday: {iso}{}]",
        if n == "end" && all {
            "; exclusive all-day end"
        } else {
            ""
        }
    )
}
fn detail_lines(e: &Value, prefix: &str, indent: &str, attachments: bool) -> String {
    let mut l = vec![
        format!(
            "{prefix}Description: {}",
            text(e, "description").unwrap_or("No Description")
        ),
        format!(
            "{prefix}Location: {}",
            text(e, "location").unwrap_or("No Location")
        ),
        format!("{prefix}Color ID: {}", text(e, "colorId").unwrap_or("None")),
    ];
    if let Some(v) = text(e, "recurringEventId") {
        l.push(format!("{prefix}Recurring Event ID: {v}"))
    }
    if let Some(v) = text(e, "eventType").filter(|v| *v != "default") {
        l.push(format!("{prefix}Event Type: {v}"))
    }
    if let Some(v) = text(e, "status").filter(|v| *v != "confirmed") {
        l.push(format!("{prefix}Status: {v}"))
    }
    if let Some(v) = e.get("creator").and_then(person) {
        l.push(format!("{prefix}Creator: {v}"))
    }
    if let Some(v) = e.get("organizer").and_then(person) {
        l.push(format!("{prefix}Organizer: {v}"))
    }
    if let Some(v) = meeting_link(e) {
        l.push(format!("{prefix}Meeting Link: {v}"))
    }
    let at = arr(e, "attendees");
    l.push(format!(
        "{prefix}Attendees: {}",
        if at.is_empty() {
            "None".into()
        } else {
            at.iter()
                .map(|v| text(v, "email").unwrap_or(""))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    l.push(format!(
        "{prefix}Attendee Details: {}",
        attendee_details(at, indent)
    ));
    if attachments {
        l.push(format!(
            "{prefix}Attachments: {}",
            attachment_details(arr(e, "attachments"), indent)
        ))
    }
    format!("{}\n", l.join("\n"))
}
fn person(v: &Value) -> Option<String> {
    let o = v.as_object()?;
    let n = o
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let e = o.get("email").and_then(Value::as_str).unwrap_or("").trim();
    match (n.is_empty(), e.is_empty()) {
        (false, false) => Some(format!("{n} <{e}>")),
        (false, true) => Some(n.into()),
        (true, false) => Some(format!("<{e}>")),
        _ => None,
    }
}
fn attendee_details(v: &[Value], indent: &str) -> String {
    if v.is_empty() {
        return "None".into();
    }
    v.iter()
        .map(|a| {
            let mut x = format!(
                "{}: {}",
                text(a, "email").unwrap_or("unknown"),
                text(a, "responseStatus").unwrap_or("unknown")
            );
            if flag(a, "organizer").unwrap_or(false) {
                x.push_str(" (organizer)")
            }
            if flag(a, "optional").unwrap_or(false) {
                x.push_str(" (optional)")
            }
            x
        })
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}
fn attachment_details(v: &[Value], indent: &str) -> String {
    if v.is_empty() {
        return "None".into();
    }
    v.iter()
        .map(|a| {
            format!(
                "{}\n{indent}File URL: {}\n{indent}File ID: {}\n{indent}MIME Type: {}",
                text(a, "title").unwrap_or("Untitled"),
                text(a, "fileUrl").unwrap_or("No URL"),
                text(a, "fileId").unwrap_or("No ID"),
                text(a, "mimeType").unwrap_or("Unknown")
            )
        })
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}
fn arr<'a>(v: &'a Value, n: &str) -> &'a [Value] {
    v.get(n)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn text<'a>(v: &'a Value, n: &str) -> Option<&'a str> {
    v.get(n).and_then(Value::as_str)
}
fn flag(v: &Value, n: &str) -> Option<bool> {
    v.get(n).and_then(Value::as_bool)
}
fn required_api<'a>(v: &'a Value, n: &str) -> Result<&'a str, String> {
    text(v, n).ok_or_else(|| format!("Unexpected error: Google Calendar response missing {n}"))
}
