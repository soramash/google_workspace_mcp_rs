use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Map, Value, json};
use time::{Date, Month};

use crate::auth::GoogleSession;

const PEOPLE_API: &str = "https://people.googleapis.com/v1";
const CONTACTS_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/contacts.readonly";
const CONTACTS_SCOPE: &str = "https://www.googleapis.com/auth/contacts";
const DEFAULT_PERSON_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,organizations";
const DETAILED_PERSON_FIELDS: &str = "names,nicknames,emailAddresses,phoneNumbers,organizations,biographies,addresses,birthdays,urls,userDefined,relations,photos,metadata,memberships";
const CONTACT_GROUP_FIELDS: &str = "name,groupType,memberCount,metadata";
const UPDATE_FIELDS: &[&str] = &[
    "names",
    "phoneNumbers",
    "emailAddresses",
    "organizations",
    "nicknames",
    "urls",
    "userDefined",
    "relations",
    "biographies",
    "addresses",
    "birthdays",
];

#[derive(Clone, Copy)]
enum Kind {
    String,
    NullableString,
    Integer,
    Boolean,
    NullableObjectArray,
    NullableStringList,
}

const SEARCH_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("query", Kind::String),
    ("page_size", Kind::Integer),
];
const GET_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("contact_id", Kind::String),
];
const LIST_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("page_size", Kind::Integer),
    ("page_token", Kind::NullableString),
    ("sort_order", Kind::NullableString),
];
const MANAGE_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("action", Kind::String),
    ("contact_id", Kind::NullableString),
    ("given_name", Kind::NullableString),
    ("family_name", Kind::NullableString),
    ("phones", Kind::NullableObjectArray),
    ("emails", Kind::NullableObjectArray),
    ("organizations", Kind::NullableObjectArray),
    ("nicknames", Kind::NullableObjectArray),
    ("urls", Kind::NullableObjectArray),
    ("user_defined", Kind::NullableObjectArray),
    ("relations", Kind::NullableObjectArray),
    ("notes", Kind::NullableString),
    ("address", Kind::NullableString),
    ("birthday", Kind::NullableString),
    ("phones_mode", Kind::String),
    ("emails_mode", Kind::String),
    ("organizations_mode", Kind::String),
    ("nicknames_mode", Kind::String),
    ("urls_mode", Kind::String),
    ("user_defined_mode", Kind::String),
    ("relations_mode", Kind::String),
    ("phone", Kind::NullableString),
    ("email", Kind::NullableString),
    ("organization", Kind::NullableString),
    ("job_title", Kind::NullableString),
];
const LIST_GROUP_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("page_size", Kind::Integer),
    ("page_token", Kind::NullableString),
];
const GET_GROUP_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("group_id", Kind::String),
    ("max_members", Kind::Integer),
];
const BATCH_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("action", Kind::String),
    ("contacts", Kind::NullableObjectArray),
    ("updates", Kind::NullableObjectArray),
    ("contact_ids", Kind::NullableStringList),
    ("field", Kind::NullableString),
];
const MANAGE_GROUP_ARGS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("action", Kind::String),
    ("group_id", Kind::NullableString),
    ("name", Kind::NullableString),
    ("delete_contacts", Kind::Boolean),
    ("add_contact_ids", Kind::NullableStringList),
    ("remove_contact_ids", Kind::NullableStringList),
];

pub async fn dispatch(tool_name: &str, arguments: &Map<String, Value>) -> Result<String, String> {
    match tool_name {
        "search_contacts" => search_contacts(arguments).await,
        "get_contact" => get_contact(arguments).await,
        "list_contacts" => list_contacts(arguments).await,
        "manage_contact" => manage_contact(arguments).await,
        "list_contact_groups" => list_contact_groups(arguments).await,
        "get_contact_group" => get_contact_group(arguments).await,
        "manage_contacts_batch" => manage_contacts_batch(arguments).await,
        "manage_contact_group" => manage_contact_group(arguments).await,
        _ => Err(format!("Unsupported native Contacts tool: {tool_name}")),
    }
}

async fn search_contacts(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, SEARCH_ARGS)?;
    let query = required_string(a, "query")?;
    let size = bounded_size(a, "page_size", 30, 30)?;
    let s = session(a, CONTACTS_READONLY_SCOPE).await?;
    // People search needs one successful cache warm-up per principal. Failure is non-fatal.
    if !search_cache_is_warm(&s.user_email)
        && send(
            &s,
            "search_contacts",
            Method::GET,
            &format!("{PEOPLE_API}/people:searchContacts"),
            &[
                ("query".into(), "".into()),
                ("readMask".into(), "names".into()),
                ("pageSize".into(), "1".into()),
            ],
            None,
        )
        .await
        .is_ok()
    {
        mark_search_cache_warm(&s.user_email);
    }
    let result = send(
        &s,
        "search_contacts",
        Method::GET,
        &format!("{PEOPLE_API}/people:searchContacts"),
        &[
            ("query".into(), query.clone()),
            ("readMask".into(), DEFAULT_PERSON_FIELDS.into()),
            ("pageSize".into(), size.to_string()),
        ],
        None,
    )
    .await?;
    let results = array(&result, "results");
    if results.is_empty() {
        return Ok(format!(
            "No contacts found matching '{query}' for {}.",
            s.user_email
        ));
    }
    let mut out = format!(
        "Search Results for '{query}' ({} found):\n\n",
        results.len()
    );
    for item in results {
        out.push_str(&format_contact(
            item.get("person").unwrap_or(&Value::Null),
            false,
        ));
        out.push_str("\n\n");
    }
    Ok(out)
}

async fn get_contact(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, GET_ARGS)?;
    let resource = resource_name(&required_string(a, "contact_id")?, "people")?;
    let s = session(a, CONTACTS_READONLY_SCOPE).await?;
    let person = send(
        &s,
        "get_contact",
        Method::GET,
        &format!("{PEOPLE_API}/{resource}"),
        &[("personFields".into(), DETAILED_PERSON_FIELDS.into())],
        None,
    )
    .await?;
    Ok(format!(
        "Contact Details for {}:\n\n{}",
        s.user_email,
        format_contact(&person, true)
    ))
}

async fn list_contacts(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, LIST_ARGS)?;
    let size = bounded_size(a, "page_size", 100, 1000)?;
    let s = session(a, CONTACTS_READONLY_SCOPE).await?;
    let mut query = vec![
        ("personFields".into(), DEFAULT_PERSON_FIELDS.into()),
        ("pageSize".into(), size.to_string()),
    ];
    push_nonempty(
        &mut query,
        "pageToken",
        optional_string(a, "page_token").as_deref(),
    );
    push_nonempty(
        &mut query,
        "sortOrder",
        optional_string(a, "sort_order").as_deref(),
    );
    let result = send(
        &s,
        "list_contacts",
        Method::GET,
        &format!("{PEOPLE_API}/people/me/connections"),
        &query,
        None,
    )
    .await?;
    let people = array(&result, "connections");
    if people.is_empty() {
        return Ok(format!("No contacts found for {}.", s.user_email));
    }
    let total = result
        .get("totalPeople")
        .and_then(Value::as_i64)
        .unwrap_or(people.len() as i64);
    let mut out = format!(
        "Contacts for {} ({} of {total}):\n\n",
        s.user_email,
        people.len()
    );
    for person in people {
        out.push_str(&format_contact(person, false));
        out.push_str("\n\n");
    }
    if let Some(token) = string(&result, "nextPageToken") {
        out.push_str(&format!("Next page token: {token}"));
    }
    Ok(out)
}

async fn manage_contact(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, MANAGE_ARGS)?;
    validate_person(a, true, false)?;
    let action = parse_action(a, &["create", "update", "delete"])?;
    for name in [
        "phones_mode",
        "emails_mode",
        "organizations_mode",
        "nicknames_mode",
        "urls_mode",
        "user_defined_mode",
        "relations_mode",
    ] {
        let mode = optional_string(a, name).unwrap_or_else(|| "merge".into());
        if !matches!(mode.as_str(), "merge" | "replace" | "remove") {
            return Err(format!(
                "Invalid {name} '{mode}'. Must be 'merge', 'replace', or 'remove'."
            ));
        }
    }
    let s = session(a, CONTACTS_SCOPE).await?;
    if action == "create" {
        let body = person_body(a, true)?;
        if body.is_empty() {
            return Err("At least one field (name, email, phone, etc.) must be provided.".into());
        }
        let result = send(
            &s,
            "manage_contact",
            Method::POST,
            &format!("{PEOPLE_API}/people:createContact"),
            &[("personFields".into(), DETAILED_PERSON_FIELDS.into())],
            Some(Value::Object(body)),
        )
        .await?;
        return Ok(format!(
            "Contact Created for {}:\n\n{}",
            s.user_email,
            format_contact(&result, true)
        ));
    }
    let id = required_for_action(a, "contact_id", &action)?;
    let resource = resource_name(&id, "people")?;
    if action == "delete" {
        send(
            &s,
            "manage_contact",
            Method::DELETE,
            &format!("{PEOPLE_API}/{resource}:deleteContact"),
            &[],
            None,
        )
        .await?;
        return Ok(format!(
            "Contact {id} has been deleted for {}.",
            s.user_email
        ));
    }
    let new_body = person_body(a, true)?;
    if new_body.is_empty() {
        return Err("At least one field (name, email, phone, etc.) must be provided.".into());
    }
    for attempt in 0..3 {
        let current = send(
            &s,
            "manage_contact",
            Method::GET,
            &format!("{PEOPLE_API}/{resource}"),
            &[("personFields".into(), DETAILED_PERSON_FIELDS.into())],
            None,
        )
        .await?;
        let etag = string(&current, "etag")
            .ok_or_else(|| "Unable to get contact etag for update.".to_string())?;
        let mut body = merge_body(&current, &new_body, a);
        body.insert("etag".into(), Value::String(etag.into()));
        let mask = UPDATE_FIELDS
            .iter()
            .filter(|f| body.contains_key(**f))
            .copied()
            .collect::<Vec<_>>()
            .join(",");
        let request = s
            .request(
                Method::PATCH,
                &format!("{PEOPLE_API}/{resource}:updateContact"),
            )
            .query(&[
                ("updatePersonFields", mask.as_str()),
                ("personFields", DETAILED_PERSON_FIELDS),
            ])
            .json(&Value::Object(body));
        match send_patch("manage_contact", request).await? {
            PatchResult::Success(result) => {
                return Ok(format!(
                    "Contact Updated for {}:\n\n{}",
                    s.user_email,
                    format_contact(&result, true)
                ));
            }
            PatchResult::Conflict if attempt < 2 => continue,
            PatchResult::Conflict => return Err(
                "API error in manage_contact: 412 Precondition Failed: Google API request failed"
                    .into(),
            ),
        }
    }
    unreachable!()
}

async fn list_contact_groups(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, LIST_GROUP_ARGS)?;
    let size = bounded_size(a, "page_size", 100, 1000)?;
    let s = session(a, CONTACTS_READONLY_SCOPE).await?;
    let mut query = vec![
        ("pageSize".into(), size.to_string()),
        ("groupFields".into(), CONTACT_GROUP_FIELDS.into()),
    ];
    push_nonempty(
        &mut query,
        "pageToken",
        optional_string(a, "page_token").as_deref(),
    );
    let result = send(
        &s,
        "list_contact_groups",
        Method::GET,
        &format!("{PEOPLE_API}/contactGroups"),
        &query,
        None,
    )
    .await?;
    let groups = array(&result, "contactGroups");
    if groups.is_empty() {
        return Ok(format!("No contact groups found for {}.", s.user_email));
    }
    let mut out = format!("Contact Groups for {}:\n\n", s.user_email);
    for group in groups {
        out.push_str(&format!(
            "- {}\n  ID: {}\n  Type: {}\n  Members: {}\n\n",
            string(group, "name").unwrap_or("Unnamed"),
            string(group, "resourceName")
                .unwrap_or("")
                .replace("contactGroups/", ""),
            string(group, "groupType").unwrap_or("USER_CONTACT_GROUP"),
            group
                .get("memberCount")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        ));
    }
    if let Some(token) = string(&result, "nextPageToken") {
        out.push_str(&format!("Next page token: {token}"));
    }
    Ok(out)
}

async fn get_contact_group(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, GET_GROUP_ARGS)?;
    let id = required_string(a, "group_id")?;
    let resource = resource_name(&id, "contactGroups")?;
    let size = bounded_size(a, "max_members", 100, 1000)?;
    let s = session(a, CONTACTS_READONLY_SCOPE).await?;
    let result = send(
        &s,
        "get_contact_group",
        Method::GET,
        &format!("{PEOPLE_API}/{resource}"),
        &[
            ("maxMembers".into(), size.to_string()),
            ("groupFields".into(), CONTACT_GROUP_FIELDS.into()),
        ],
        None,
    )
    .await?;
    let members = array(&result, "memberResourceNames");
    let mut out = format!(
        "Contact Group Details for {}:\n\nName: {}\nID: {id}\nType: {}\nTotal Members: {}\n",
        s.user_email,
        string(&result, "name").unwrap_or("Unnamed"),
        string(&result, "groupType").unwrap_or("USER_CONTACT_GROUP"),
        result
            .get("memberCount")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    );
    if !members.is_empty() {
        out.push_str(&format!("\nMembers ({} shown):\n", members.len()));
        for member in members {
            if let Some(name) = member.as_str() {
                out.push_str(&format!("  - {}\n", name.replace("people/", "")));
            }
        }
    }
    Ok(out)
}

async fn manage_contacts_batch(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, BATCH_ARGS)?;
    validate_batch(a)?;
    let action = parse_action(a, &["create", "update", "delete"])?;
    let s = session(a, CONTACTS_SCOPE).await?;
    match action.as_str() {
        "create" => {
            let contacts = objects(a, "contacts")?
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "contacts parameter is required for 'create' action.".to_string())?;
            if contacts.len() > 200 {
                return Err("Maximum 200 contacts can be created in a batch.".into());
            }
            let mut bodies = Vec::new();
            for contact in contacts {
                let body = person_body(contact, false)?;
                if !body.is_empty() {
                    bodies.push(json!({"contactPerson": body}));
                }
            }
            if bodies.is_empty() {
                return Err("No valid contact data provided.".into());
            }
            let result = send(
                &s,
                "manage_contacts_batch",
                Method::POST,
                &format!("{PEOPLE_API}/people:batchCreateContacts"),
                &[],
                Some(json!({"contacts": bodies, "readMask": DEFAULT_PERSON_FIELDS})),
            )
            .await?;
            let created = array(&result, "createdPeople");
            let mut out = format!(
                "Batch Create Results for {}:\n\nCreated {} contacts:\n\n",
                s.user_email,
                created.len()
            );
            for item in created {
                out.push_str(&format_contact(
                    item.get("person").unwrap_or(&Value::Null),
                    false,
                ));
                out.push_str("\n\n");
            }
            Ok(out)
        }
        "update" => batch_update(a, &s).await,
        "delete" => {
            let ids = string_list(a, "contact_ids")?
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    "contact_ids parameter is required for 'delete' action.".to_string()
                })?;
            if ids.len() > 500 {
                return Err("Maximum 500 contacts can be deleted in a batch.".into());
            }
            let names = ids
                .iter()
                .map(|id| resource_name(id, "people"))
                .collect::<Result<Vec<_>, _>>()?;
            send(
                &s,
                "manage_contacts_batch",
                Method::POST,
                &format!("{PEOPLE_API}/people:batchDeleteContacts"),
                &[],
                Some(json!({"resourceNames": names})),
            )
            .await?;
            Ok(format!(
                "Batch deleted {} contacts for {}.",
                ids.len(),
                s.user_email
            ))
        }
        _ => unreachable!(),
    }
}

async fn batch_update(a: &Map<String, Value>, s: &GoogleSession) -> Result<String, String> {
    let updates = objects(a, "updates")?
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "updates parameter is required for 'update' action.".to_string())?;
    if updates.len() > 200 {
        return Err("Maximum 200 contacts can be updated in a batch.".into());
    }
    let field = optional_string(a, "field").ok_or_else(|| "field parameter is required for batch 'update' action. Must be one of: names, phoneNumbers, emailAddresses, organizations, nicknames, urls, userDefined, relations, biographies, addresses, birthdays. Use a single field per batch call to avoid unintentional data loss from a union updateMask.".to_string())?;
    if !UPDATE_FIELDS.contains(&field.as_str()) {
        let mut valid = UPDATE_FIELDS.to_vec();
        valid.sort_unstable();
        return Err(format!(
            "Invalid field '{field}'. Must be one of: {}.",
            valid.join(", ")
        ));
    }
    let resources = updates
        .iter()
        .map(|u| resource_name(required_nested(u, "contact_id")?, "people"))
        .collect::<Result<Vec<_>, String>>()?;
    let mut query = vec![("personFields".into(), "metadata".into())];
    for resource in &resources {
        query.push(("resourceNames".into(), resource.clone()));
    }
    let fetched = send(
        s,
        "manage_contacts_batch",
        Method::GET,
        &format!("{PEOPLE_API}/people:batchGet"),
        &query,
        None,
    )
    .await?;
    let mut etags = HashMap::new();
    for response in array(&fetched, "responses") {
        if let Some(person) = response.get("person")
            && let (Some(name), Some(etag)) =
                (string(person, "resourceName"), string(person, "etag"))
        {
            etags.insert(name.to_string(), etag.to_string());
        }
    }
    let mut contacts = Map::new();
    for (update, resource) in updates.iter().zip(resources) {
        let Some(etag) = etags.get(&resource) else {
            continue;
        };
        let body = person_body(update, false)?;
        let Some(value) = body.get(&field) else {
            continue;
        };
        contacts.insert(resource, json!({"etag": etag, field.clone(): value}));
    }
    if contacts.is_empty() {
        return Err("No valid update data provided.".into());
    }
    let result = send(
        s,
        "manage_contacts_batch",
        Method::POST,
        &format!("{PEOPLE_API}/people:batchUpdateContacts"),
        &[],
        Some(json!({"contacts": contacts, "updateMask": field, "readMask": DEFAULT_PERSON_FIELDS})),
    )
    .await?;
    let results = result
        .get("updateResult")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = format!(
        "Batch Update Results for {}:\n\nUpdated {} contacts:\n\n",
        s.user_email,
        results.len()
    );
    for item in results.values() {
        out.push_str(&format_contact(
            item.get("person").unwrap_or(&Value::Null),
            false,
        ));
        out.push_str("\n\n");
    }
    Ok(out)
}

async fn manage_contact_group(a: &Map<String, Value>) -> Result<String, String> {
    validate_args(a, MANAGE_GROUP_ARGS)?;
    let action = parse_action(a, &["create", "update", "delete", "modify_members"])?;
    let s = session(a, CONTACTS_SCOPE).await?;
    if action == "create" {
        let name = required_for_action(a, "name", "create")?;
        let result = send(
            &s,
            "manage_contact_group",
            Method::POST,
            &format!("{PEOPLE_API}/contactGroups"),
            &[],
            Some(json!({"contactGroup": {"name": name}})),
        )
        .await?;
        return Ok(format!(
            "Contact Group Created for {}:\n\nName: {}\nID: {}\nType: {}\n",
            s.user_email,
            string(&result, "name").unwrap_or(&name),
            string(&result, "resourceName")
                .unwrap_or("")
                .replace("contactGroups/", ""),
            string(&result, "groupType").unwrap_or("USER_CONTACT_GROUP")
        ));
    }
    let id = required_for_action(a, "group_id", &action)?;
    let resource = resource_name(&id, "contactGroups")?;
    match action.as_str() {
        "update" => {
            let name = required_for_action(a, "name", "update")?;
            let result = send(
                &s,
                "manage_contact_group",
                Method::PUT,
                &format!("{PEOPLE_API}/{resource}"),
                &[],
                Some(json!({"contactGroup": {"name": name}})),
            )
            .await?;
            Ok(format!(
                "Contact Group Updated for {}:\n\nName: {}\nID: {id}\n",
                s.user_email,
                string(&result, "name").unwrap_or(&name)
            ))
        }
        "delete" => {
            let delete = optional_bool(a, "delete_contacts", false);
            send(
                &s,
                "manage_contact_group",
                Method::DELETE,
                &format!("{PEOPLE_API}/{resource}"),
                &[("deleteContacts".into(), delete.to_string())],
                None,
            )
            .await?;
            Ok(format!(
                "Contact group {id} has been deleted for {}.{}",
                s.user_email,
                if delete {
                    " Contacts in the group were also deleted."
                } else {
                    " Contacts in the group were preserved."
                }
            ))
        }
        "modify_members" => {
            let add = string_list(a, "add_contact_ids")?.unwrap_or_default();
            let remove = string_list(a, "remove_contact_ids")?.unwrap_or_default();
            if add.is_empty() && remove.is_empty() {
                return Err(
                    "At least one of add_contact_ids or remove_contact_ids must be provided."
                        .into(),
                );
            }
            let mut body = Map::new();
            if !add.is_empty() {
                body.insert(
                    "resourceNamesToAdd".into(),
                    Value::Array(
                        add.iter()
                            .map(|id| resource_name(id, "people").map(Value::String))
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                );
            }
            if !remove.is_empty() {
                body.insert(
                    "resourceNamesToRemove".into(),
                    Value::Array(
                        remove
                            .iter()
                            .map(|id| resource_name(id, "people").map(Value::String))
                            .collect::<Result<Vec<_>, _>>()?,
                    ),
                );
            }
            let result = send(
                &s,
                "manage_contact_group",
                Method::POST,
                &format!("{PEOPLE_API}/{resource}/members:modify"),
                &[],
                Some(Value::Object(body)),
            )
            .await?;
            let mut out = format!(
                "Contact Group Members Modified for {}:\n\nGroup: {id}\n",
                s.user_email
            );
            if !add.is_empty() {
                out.push_str(&format!("Added: {} contacts\n", add.len()));
            }
            if !remove.is_empty() {
                out.push_str(&format!("Removed: {} contacts\n", remove.len()));
            }
            append_array(&mut out, &result, "notFoundResourceNames", "Not found");
            append_array(
                &mut out,
                &result,
                "canNotRemoveLastContactGroupResourceNames",
                "Cannot remove (last group)",
            );
            Ok(out)
        }
        _ => unreachable!(),
    }
}

fn person_body(input: &Map<String, Value>, top: bool) -> Result<Map<String, Value>, String> {
    let mut body = Map::new();
    let given = input_value(input, "given_name", top).and_then(Value::as_str);
    let family = input_value(input, "family_name", top).and_then(Value::as_str);
    if given.is_some_and(|v| !v.is_empty()) || family.is_some_and(|v| !v.is_empty()) {
        body.insert(
            "names".into(),
            json!([{"givenName": given.unwrap_or(""), "familyName": family.unwrap_or("")}]),
        );
    }
    let emails = input_value(input, "emails", top);
    let email = input_value(input, "email", top).and_then(Value::as_str);
    if emails.is_some_and(|v| !v.is_null()) {
        body.insert("emailAddresses".into(), convert_emails(emails.unwrap())?);
    } else if let Some(email) = email {
        body.insert(
            "emailAddresses".into(),
            json!([{"value": email, "type": "other"}]),
        );
    }
    let phones = input_value(input, "phones", top);
    let phone = input_value(input, "phone", top).and_then(Value::as_str);
    if phones.is_some_and(|v| !v.is_null()) {
        body.insert("phoneNumbers".into(), convert_phones(phones.unwrap())?);
    } else if let Some(phone) = phone {
        body.insert(
            "phoneNumbers".into(),
            json!([{"value": phone, "type": "mobile"}]),
        );
    }
    let orgs = input_value(input, "organizations", top);
    if orgs.is_some_and(|v| !v.is_null()) {
        body.insert("organizations".into(), convert_orgs(orgs.unwrap())?);
    } else {
        let name = input_value(input, "organization", top).and_then(Value::as_str);
        let title = input_value(input, "job_title", top).and_then(Value::as_str);
        if name.is_some() || title.is_some() {
            let mut org = Map::new();
            if let Some(v) = name {
                org.insert("name".into(), Value::String(v.into()));
            }
            if let Some(v) = title {
                org.insert("title".into(), Value::String(v.into()));
            }
            body.insert(
                "organizations".into(),
                Value::Array(vec![Value::Object(org)]),
            );
        }
    }
    for (source, target, converter) in [
        (
            "nicknames",
            "nicknames",
            convert_nicknames as fn(&Value) -> Result<Value, String>,
        ),
        ("urls", "urls", convert_urls),
        ("user_defined", "userDefined", convert_user_defined),
        ("relations", "relations", convert_relations),
    ] {
        if let Some(value) = input_value(input, source, top)
            && !value.is_null()
        {
            body.insert(target.into(), converter(value)?);
        }
    }
    if let Some(notes) = input_value(input, "notes", top).and_then(Value::as_str) {
        body.insert(
            "biographies".into(),
            if notes.is_empty() {
                Value::Array(vec![])
            } else {
                json!([{"value": notes, "contentType": "TEXT_PLAIN"}])
            },
        );
    }
    if let Some(address) = input_value(input, "address", top).and_then(Value::as_str)
        && !address.is_empty()
    {
        body.insert("addresses".into(), json!([{"formattedValue": address}]));
    }
    if let Some(birthday) = input_value(input, "birthday", top).and_then(Value::as_str) {
        body.insert(
            "birthdays".into(),
            if birthday.trim().is_empty() || birthday.trim().eq_ignore_ascii_case("clear") {
                Value::Array(vec![])
            } else {
                Value::Array(vec![parse_birthday(birthday)?])
            },
        );
    }
    Ok(body)
}

fn convert_phones(v: &Value) -> Result<Value, String> {
    convert_entries(v, |m| {
        let value = m
            .get("number")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .or_else(|| m.get("value").and_then(Value::as_str))
            .unwrap_or("");
        Ok((!value.is_empty()).then(|| typed(value, m)))
    })
}
fn convert_emails(v: &Value) -> Result<Value, String> {
    convert_entries(v, |m| {
        let value = m
            .get("address")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .or_else(|| m.get("value").and_then(Value::as_str))
            .unwrap_or("");
        Ok((!value.is_empty()).then(|| typed(value, m)))
    })
}
fn typed(value: &str, m: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("value".into(), Value::String(value.into()));
    if let Some(t) = m
        .get("type")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            m.get("label")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        })
    {
        out.insert("type".into(), Value::String(t.into()));
    }
    Value::Object(out)
}
fn convert_orgs(v: &Value) -> Result<Value, String> {
    convert_entries(v, |m| {
        let mut out = Map::new();
        for k in ["name", "title", "department", "type"] {
            if let Some(v) = m.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()) {
                out.insert(k.into(), Value::String(v.into()));
            }
        }
        if let Some(v) = m
            .get("jobDescription")
            .or_else(|| m.get("description"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            out.insert("jobDescription".into(), Value::String(v.into()));
        }
        Ok((!out.is_empty()).then_some(Value::Object(out)))
    })
}
fn convert_nicknames(v: &Value) -> Result<Value, String> {
    convert_named(v, "value", "value")
}
fn convert_urls(v: &Value) -> Result<Value, String> {
    convert_named(v, "value", "value")
}
fn convert_relations(v: &Value) -> Result<Value, String> {
    convert_named(v, "person", "person")
}
fn convert_user_defined(v: &Value) -> Result<Value, String> {
    convert_entries(v, |m| {
        let key = m.get("key").and_then(Value::as_str).unwrap_or("").trim();
        if key.is_empty() {
            return Ok(None);
        }
        let mut out = Map::new();
        out.insert("key".into(), Value::String(key.into()));
        if let Some(value) = m
            .get("value")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            out.insert("value".into(), Value::String(value.into()));
        }
        Ok(Some(Value::Object(out)))
    })
}
fn convert_named(v: &Value, input: &str, output: &str) -> Result<Value, String> {
    convert_entries(v, |m| {
        let text = m.get(input).and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() {
            return Ok(None);
        }
        let mut out = Map::new();
        out.insert(output.into(), Value::String(text.into()));
        if let Some(t) = m
            .get("type")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            out.insert("type".into(), Value::String(t.into()));
        }
        Ok(Some(Value::Object(out)))
    })
}
fn convert_entries<F>(v: &Value, mut f: F) -> Result<Value, String>
where
    F: FnMut(&Map<String, Value>) -> Result<Option<Value>, String>,
{
    let values = v
        .as_array()
        .ok_or_else(|| "contact field must be an array".to_string())?;
    let mut out = Vec::new();
    for value in values {
        let item = value
            .as_object()
            .ok_or_else(|| "contact field items must be objects".to_string())?;
        if let Some(value) = f(item)? {
            out.push(value);
        }
    }
    Ok(Value::Array(out))
}

fn parse_birthday(input: &str) -> Result<Value, String> {
    let p = input.trim().split('-').collect::<Vec<_>>();
    let (year, month, day) = match p.as_slice() {
        [m, d] => (None, m.parse::<i32>(), d.parse::<u8>()),
        [y, m, d] => (Some(y.parse::<i32>()), m.parse::<i32>(), d.parse::<u8>()),
        _ => {
            return Err(format!(
                "Invalid birthday format '{input}'. Use 'YYYY-MM-DD' or 'MM-DD'."
            ));
        }
    };
    let year = match year {
        Some(Ok(v @ 1..=9999)) => Some(v),
        Some(_) => {
            return Err(format!(
                "Invalid birthday '{input}': not a real calendar date."
            ));
        }
        None => None,
    };
    let (Ok(month), Ok(day)) = (month, day) else {
        return Err(format!(
            "Invalid birthday format '{input}'. Use 'YYYY-MM-DD' or 'MM-DD'."
        ));
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(format!(
            "Invalid birthday '{input}': month must be 1-12 and day must be 1-31."
        ));
    }
    let m = Month::try_from(month as u8)
        .map_err(|_| format!("Invalid birthday '{input}': not a real calendar date."))?;
    Date::from_calendar_date(year.unwrap_or(2000), m, day)
        .map_err(|_| format!("Invalid birthday '{input}': not a real calendar date."))?;
    let mut d = Map::new();
    d.insert("month".into(), json!(month));
    d.insert("day".into(), json!(day));
    if let Some(y) = year {
        d.insert("year".into(), json!(y));
    }
    Ok(json!({"date":d}))
}

fn merge_body(
    current: &Value,
    new: &Map<String, Value>,
    a: &Map<String, Value>,
) -> Map<String, Value> {
    let mut out = new.clone();
    for (field, mode_name) in [
        ("phoneNumbers", "phones_mode"),
        ("emailAddresses", "emails_mode"),
        ("organizations", "organizations_mode"),
        ("nicknames", "nicknames_mode"),
        ("urls", "urls_mode"),
        ("userDefined", "user_defined_mode"),
        ("relations", "relations_mode"),
    ] {
        if let Some(values) = new.get(field).and_then(Value::as_array) {
            let existing = current
                .get(field)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mode = optional_string(a, mode_name).unwrap_or_else(|| "merge".into());
            out.insert(
                field.into(),
                Value::Array(merge_values(field, existing, values, &mode)),
            );
        }
    }
    out
}
fn merge_values(field: &str, existing: &[Value], new: &[Value], mode: &str) -> Vec<Value> {
    if mode == "replace" {
        return new.to_vec();
    }
    if field == "userDefined" {
        return merge_ud(existing, new, mode);
    }
    let key = |v: &Value, remove: bool| merge_key(field, v, remove);
    if mode == "remove" {
        let remove = new.iter().map(|v| key(v, true)).collect::<HashSet<_>>();
        return existing
            .iter()
            .filter(|v| !remove.contains(&key(v, true)))
            .cloned()
            .collect();
    }
    let mut out = existing.to_vec();
    let mut seen = existing
        .iter()
        .map(|v| key(v, false))
        .collect::<HashSet<_>>();
    for v in new {
        if seen.insert(key(v, false)) {
            out.push(v.clone());
        }
    }
    out
}
fn merge_ud(existing: &[Value], new: &[Value], mode: &str) -> Vec<Value> {
    let replacements = new
        .iter()
        .map(|v| (normalized(v, "key"), v.clone()))
        .collect::<HashMap<_, _>>();
    if mode == "remove" {
        return existing
            .iter()
            .filter(|v| !replacements.contains_key(&normalized(v, "key")))
            .cloned()
            .collect();
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for v in existing {
        let k = normalized(v, "key");
        out.push(replacements.get(&k).cloned().unwrap_or_else(|| v.clone()));
        seen.insert(k);
    }
    for (k, v) in replacements {
        if !seen.contains(&k) {
            out.push(v);
        }
    }
    out
}
fn merge_key(field: &str, v: &Value, removal: bool) -> String {
    match field {
        "phoneNumbers" => {
            let source = if removal {
                v.get("value")
            } else {
                v.get("canonicalForm").or_else(|| v.get("value"))
            };
            source
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '+')
                .collect::<String>()
                .to_lowercase()
        }
        "emailAddresses" | "nicknames" => normalized(v, "value"),
        "urls" => {
            let normalized = normalized(v, "value");
            normalized
                .strip_suffix('/')
                .unwrap_or(&normalized)
                .to_string()
        }
        "organizations" => ["name", "title", "department", "jobDescription", "type"]
            .iter()
            .map(|k| {
                if *k == "jobDescription" {
                    v.get(*k)
                        .or_else(|| v.get("description"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_lowercase()
                } else {
                    normalized(v, k)
                }
            })
            .collect::<Vec<_>>()
            .join("\u{1f}"),
        "relations" => format!(
            "{}\u{1f}{}",
            normalized(v, "person"),
            v.get("formattedType")
                .or_else(|| v.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_lowercase()
        ),
        _ => String::new(),
    }
}
fn normalized(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

fn format_contact(person: &Value, detailed: bool) -> String {
    let mut lines = vec![format!(
        "Contact ID: {}",
        string(person, "resourceName")
            .unwrap_or("Unknown")
            .replace("people/", "")
    )];
    if let Some(name) = array(person, "names")
        .first()
        .and_then(|v| string(v, "displayName"))
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("Name: {name}"));
    }
    let nick = values(person, "nicknames", "value");
    if !nick.is_empty() {
        lines.push(format!("Nicknames: {}", nick.join(", ")));
    }
    append_typed(
        &mut lines,
        person,
        "emailAddresses",
        "Email",
        "Emails",
        false,
    );
    append_typed(&mut lines, person, "phoneNumbers", "Phone", "Phones", true);
    if let Some(org) = array(person, "organizations").first() {
        let mut p = Vec::new();
        if let Some(v) = string(org, "title").filter(|v| !v.is_empty()) {
            p.push(v.to_string());
        }
        if let Some(v) = string(org, "name").filter(|v| !v.is_empty()) {
            p.push(format!("at {v}"));
        }
        if !p.is_empty() {
            lines.push(format!("Organization: {}", p.join(" ")));
        }
    }
    if detailed {
        if let Some(v) = array(person, "addresses")
            .first()
            .and_then(|v| string(v, "formattedValue"))
            .filter(|v| !v.is_empty())
        {
            lines.push(format!("Address: {v}"));
        }
        if let Some(date) = array(person, "birthdays")
            .first()
            .and_then(|v| v.get("date"))
        {
            let m = number(date.get("month"));
            let d = number(date.get("day"));
            lines.push(format!(
                "Birthday: {}",
                if let Some(y) = date.get("year").and_then(Value::as_i64) {
                    format!("{y}/{m}/{d}")
                } else {
                    format!("{m}/{d}")
                }
            ));
        }
        let urls = values(person, "urls", "value");
        if !urls.is_empty() {
            lines.push(format!("URLs: {}", urls.join(", ")));
        }
        let custom = array(person, "userDefined")
            .iter()
            .filter_map(|v| Some((string(v, "key")?, string(v, "value")?)))
            .filter(|(k, v)| !k.is_empty() && !v.is_empty())
            .collect::<Vec<_>>();
        if !custom.is_empty() {
            lines.push("Custom Fields:".into());
            for (k, v) in custom {
                lines.push(format!("  - {k}: {v}"));
            }
        }
        let rels = array(person, "relations")
            .iter()
            .filter_map(|v| {
                string(v, "person").filter(|p| !p.is_empty()).map(|p| {
                    (
                        p,
                        string(v, "formattedType")
                            .or_else(|| string(v, "type"))
                            .unwrap_or(""),
                    )
                })
            })
            .collect::<Vec<_>>();
        if !rels.is_empty() {
            lines.push("Relations:".into());
            for (p, t) in rels {
                lines.push(if t.is_empty() {
                    format!("  - {p}")
                } else {
                    format!("  - {p} ({t})")
                });
            }
        }
        if let Some(bio) = array(person, "biographies")
            .first()
            .and_then(|v| string(v, "value"))
            .filter(|v| !v.is_empty())
        {
            let mut c = bio.chars();
            let short = c.by_ref().take(200).collect::<String>();
            lines.push(format!(
                "Notes: {}",
                if c.next().is_some() {
                    format!("{short}...")
                } else {
                    short
                }
            ));
        }
        let sources = person
            .get("metadata")
            .map(|v| array(v, "sources"))
            .unwrap_or_default()
            .iter()
            .filter_map(|v| string(v, "type"))
            .collect::<Vec<_>>();
        if !sources.is_empty() {
            lines.push(format!("Sources: {}", sources.join(", ")));
        }
    }
    lines.join("\n")
}
fn append_typed(
    lines: &mut Vec<String>,
    person: &Value,
    field: &str,
    singular: &str,
    plural: &str,
    phone: bool,
) {
    let vals = array(person, field)
        .iter()
        .filter(|v| string(v, "value").is_some_and(|s| !s.is_empty()))
        .map(|v| {
            let value = string(v, "value").unwrap_or("");
            let kind = if phone && string(v, "type") == Some("internal") {
                "Internal"
            } else {
                string(v, "formattedType")
                    .filter(|v| !v.is_empty())
                    .or_else(|| string(v, "type"))
                    .unwrap_or("")
            };
            if kind.is_empty() {
                value.to_string()
            } else {
                format!("{value} ({kind})")
            }
        })
        .collect::<Vec<_>>();
    match vals.as_slice() {
        [] => {}
        [one] => lines.push(format!("{singular}: {one}")),
        many => {
            lines.push(format!("{plural}:"));
            for v in many {
                lines.push(format!("  - {v}"));
            }
        }
    }
}

fn validate_args(a: &Map<String, Value>, allowed: &[(&str, Kind)]) -> Result<(), String> {
    for (key, value) in a {
        let Some((canonical, kind)) = allowed
            .iter()
            .find(|(name, _)| key == *name || key == &snake_to_camel(name))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        let camel = snake_to_camel(canonical);
        if key == &camel && camel != *canonical && a.contains_key(*canonical) {
            return Err(format!("Unexpected argument: {key}"));
        }
        let valid = match kind {
            Kind::String => value.is_string(),
            Kind::NullableString => value.is_null() || value.is_string(),
            Kind::Integer => value.as_i64().is_some(),
            Kind::Boolean => value.is_boolean(),
            Kind::NullableObjectArray => {
                value.is_null()
                    || value
                        .as_array()
                        .is_some_and(|v| v.iter().all(Value::is_object))
            }
            Kind::NullableStringList => value.is_null() || valid_list(value),
        };
        if !valid {
            return Err(format!("{canonical} must be {}", expected(*kind)));
        }
    }
    Ok(())
}
fn validate_batch(a: &Map<String, Value>) -> Result<(), String> {
    if let Some(items) = objects(a, "contacts")? {
        for item in items {
            validate_person(item, false, false)?;
        }
    }
    if let Some(items) = objects(a, "updates")? {
        for item in items {
            validate_person(item, false, true)?;
        }
    }
    Ok(())
}
fn validate_person(input: &Map<String, Value>, top: bool, update: bool) -> Result<(), String> {
    let scalar = [
        "given_name",
        "family_name",
        "notes",
        "address",
        "birthday",
        "phone",
        "email",
        "organization",
        "job_title",
    ];
    let arrays = [
        "phones",
        "emails",
        "organizations",
        "nicknames",
        "urls",
        "user_defined",
        "relations",
    ];
    let mut allowed = scalar
        .iter()
        .chain(arrays.iter())
        .copied()
        .collect::<HashSet<_>>();
    if update {
        allowed.insert("contact_id");
    }
    if top {
        allowed.extend([
            "user_google_email",
            "action",
            "contact_id",
            "phones_mode",
            "emails_mode",
            "organizations_mode",
            "nicknames_mode",
            "urls_mode",
            "user_defined_mode",
            "relations_mode",
        ]);
    }
    for (key, value) in input {
        let canonical = if top {
            allowed
                .iter()
                .find(|name| key == **name || key == &snake_to_camel(name))
                .copied()
        } else {
            allowed.contains(key.as_str()).then_some(key.as_str())
        };
        let Some(name) = canonical else {
            return Err(format!("Unexpected argument: {key}"));
        };
        if arrays.contains(&name) {
            if !value.is_null() {
                validate_entries(
                    name,
                    value
                        .as_array()
                        .ok_or_else(|| format!("{name} must be an array"))?,
                )?;
            }
        } else if !value.is_null() && !value.is_string() {
            return Err(format!("{name} must be a string"));
        }
    }
    if update {
        required_nested(input, "contact_id")?;
    }
    Ok(())
}
fn validate_entries(kind: &str, values: &[Value]) -> Result<(), String> {
    let (allowed, required): (&[&str], Option<&str>) = match kind {
        "phones" => (&["number", "value", "type"], None),
        "emails" => (&["address", "value", "type"], None),
        "organizations" => (
            &["name", "title", "department", "jobDescription", "type"],
            None,
        ),
        "nicknames" | "urls" => (&["value", "type"], Some("value")),
        "user_defined" => (&["key", "value"], Some("key")),
        "relations" => (&["person", "type"], Some("person")),
        _ => unreachable!(),
    };
    for (index, value) in values.iter().enumerate() {
        let item = value
            .as_object()
            .ok_or_else(|| format!("{kind}[{index}] must be an object"))?;
        for (key, value) in item {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("Unexpected argument: {kind}[{index}].{key}"));
            }
            let nullable = match kind {
                "phones" | "emails" | "organizations" => true,
                "nicknames" | "urls" | "relations" => key == "type",
                "user_defined" => false,
                _ => false,
            };
            if !value.is_string() && !(nullable && value.is_null()) {
                return Err(format!("{kind}[{index}].{key} must be a string"));
            }
        }
        if let Some(r) = required
            && !item.get(r).is_some_and(Value::is_string)
        {
            return Err(format!("{kind}[{index}].{r} is required"));
        }
    }
    Ok(())
}

static SEARCH_CACHE_WARMED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn search_cache_is_warm(email: &str) -> bool {
    SEARCH_CACHE_WARMED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .is_ok_and(|cache| cache.contains(email))
}

fn mark_search_cache_warm(email: &str) {
    if let Ok(mut cache) = SEARCH_CACHE_WARMED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
    {
        cache.insert(email.to_string());
    }
}

async fn session(a: &Map<String, Value>, scope: &str) -> Result<GoogleSession, String> {
    let supplied = required_string(a, "user_google_email")?;
    let s = match GoogleSession::authenticate(&[scope]).await {
        Ok(session) => session,
        Err(error)
            if scope == CONTACTS_READONLY_SCOPE
                && error.contains("do not include the required Google scopes") =>
        {
            GoogleSession::authenticate(&[CONTACTS_SCOPE]).await?
        }
        Err(error) => return Err(error),
    };
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
    query: &[(String, String)],
    body: Option<Value>,
) -> Result<Value, String> {
    let mut req = s.request(method, url).query(query);
    if let Some(body) = body {
        req = req.json(&body);
    }
    s.send_json(tool, req).await
}
enum PatchResult {
    Success(Value),
    Conflict,
}
async fn send_patch(tool: &str, request: RequestBuilder) -> Result<PatchResult, String> {
    let response = request
        .send()
        .await
        .map_err(|_| format!("API error in {tool}: Google API request failed"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| format!("API error in {tool}: failed to read Google API response"))?;
    if status == StatusCode::PRECONDITION_FAILED {
        return Ok(PatchResult::Conflict);
    }
    if !status.is_success() {
        return Err(api_error(tool, status, &body));
    }
    if body.trim().is_empty() {
        return Ok(PatchResult::Success(Value::Null));
    }
    serde_json::from_str(&body)
        .map(PatchResult::Success)
        .map_err(|_| format!("API error in {tool}: Google API returned invalid JSON"))
}
fn api_error(tool: &str, status: StatusCode, body: &str) -> String {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return format!(
            "API error in {tool}: Google authorization failed with status {status}. Re-authentication may be required."
        );
    }
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Google API request failed".into());
    format!("API error in {tool}: {status}: {message}")
}
fn resource_name(input: &str, prefix: &str) -> Result<String, String> {
    let value = input.strip_prefix(&format!("{prefix}/")).unwrap_or(input);
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
    {
        return Err(format!("Invalid {prefix} resource name"));
    }
    Ok(format!("{prefix}/{value}"))
}
fn parse_action(a: &Map<String, Value>, valid: &[&str]) -> Result<String, String> {
    let value = required_string(a, "action")?.trim().to_lowercase();
    if !valid.contains(&value.as_str()) {
        let choices = valid.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>();
        return Err(format!(
            "Invalid action '{value}'. Must be {}.",
            choices.join(", ")
        ));
    }
    Ok(value)
}
fn bounded_size(a: &Map<String, Value>, name: &str, default: i64, max: i64) -> Result<i64, String> {
    let value = argument(a, name).and_then(Value::as_i64).unwrap_or(default);
    if value < 1 {
        return Err(format!("{name} must be >= 1"));
    }
    Ok(value.min(max))
}
fn required_for_action(a: &Map<String, Value>, name: &str, action: &str) -> Result<String, String> {
    optional_string(a, name)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{name} is required for '{action}' action."))
}
fn required_string(a: &Map<String, Value>, name: &str) -> Result<String, String> {
    argument(a, name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{name} is required"))
}
fn required_nested<'a>(a: &'a Map<String, Value>, name: &str) -> Result<&'a str, String> {
    a.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Each update must include a {name}."))
}
fn optional_string(a: &Map<String, Value>, name: &str) -> Option<String> {
    argument(a, name)
        .and_then(Value::as_str)
        .map(str::to_string)
}
fn optional_bool(a: &Map<String, Value>, name: &str, default: bool) -> bool {
    argument(a, name)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}
fn argument<'a>(a: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    a.get(name).or_else(|| a.get(&snake_to_camel(name)))
}
fn input_value<'a>(a: &'a Map<String, Value>, name: &str, top: bool) -> Option<&'a Value> {
    if top { argument(a, name) } else { a.get(name) }
}
fn objects<'a>(
    a: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<Vec<&'a Map<String, Value>>>, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(v)) => v
            .iter()
            .map(|v| {
                v.as_object()
                    .ok_or_else(|| format!("{name} items must be objects"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{name} must be an array")),
    }
}
fn string_list(a: &Map<String, Value>, name: &str) -> Result<Option<Vec<String>>, String> {
    match argument(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(v)) => v
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{name} must contain only strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(Value::String(v)) => {
            let parsed: Value = serde_json::from_str(v).map_err(|_| {
                format!("{name} must be an array of strings or a JSON-encoded array of strings")
            })?;
            parsed
                .as_array()
                .ok_or_else(|| {
                    format!("{name} must be an array of strings or a JSON-encoded array of strings")
                })?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{name} must contain only strings"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        }
        Some(_) => Err(format!(
            "{name} must be an array of strings or a JSON-encoded array of strings"
        )),
    }
}
fn valid_list(v: &Value) -> bool {
    match v {
        Value::Array(v) => v.iter().all(Value::is_string),
        Value::String(v) => serde_json::from_str::<Value>(v)
            .ok()
            .and_then(|v| v.as_array().map(|a| a.iter().all(Value::is_string)))
            .unwrap_or(false),
        _ => false,
    }
}
fn expected(k: Kind) -> &'static str {
    match k {
        Kind::String | Kind::NullableString => "a string",
        Kind::Integer => "an integer",
        Kind::Boolean => "a boolean",
        Kind::NullableObjectArray => "an array of objects",
        Kind::NullableStringList => "an array of strings or a JSON-encoded array of strings",
    }
}
fn snake_to_camel(value: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in value.chars() {
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
fn push_nonempty(q: &mut Vec<(String, String)>, key: &str, value: Option<&str>) {
    if let Some(v) = value.filter(|v| !v.is_empty()) {
        q.push((key.into(), v.into()));
    }
}
fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn string<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}
fn values(v: &Value, array_key: &str, key: &str) -> Vec<String> {
    array(v, array_key)
        .iter()
        .filter_map(|v| string(v, key).filter(|v| !v.is_empty()).map(str::to_string))
        .collect()
}
fn number(v: Option<&Value>) -> String {
    v.and_then(Value::as_i64)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "?".into())
}
fn append_array(out: &mut String, result: &Value, field: &str, label: &str) {
    let values = array(result, field)
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !values.is_empty() {
        out.push_str(&format!("\n{label}: {}\n", values.join(", ")));
    }
}
