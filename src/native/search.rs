use std::env;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::auth;

const SEARCH_ENDPOINT: &str = "https://customsearch.googleapis.com/customsearch/v1";
const SEARCH_ARGUMENTS: &[&str] = &[
    "user_google_email",
    "q",
    "num",
    "start",
    "safe",
    "search_type",
    "site_search",
    "site_search_filter",
    "date_restrict",
    "file_type",
    "language",
    "country",
    "sites",
];

pub async fn search_custom(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, SEARCH_ARGUMENTS)?;
    validate_optional_enum(arguments, "safe", &["active", "moderate", "off"])?;
    validate_optional_enum(arguments, "search_type", &["image"])?;
    validate_optional_enum(arguments, "site_search_filter", &["e", "i"])?;

    let user_email = required_user_email(arguments)?;
    let mut query = required_string(arguments, "q")?;
    let num = integer(arguments, "num", 10)?;
    let start = integer(arguments, "start", 1)?;
    let safe = string(arguments, "safe").unwrap_or_else(|| String::from("off"));

    if let Some(sites) = string_list(arguments, "sites")?
        && !sites.is_empty()
    {
        let site_query = sites
            .iter()
            .map(|site| format!("site:{site}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        query = format!("{query} ({site_query})");
    }

    let (api_key, engine_id) = credentials()?;
    let mut parameters = vec![
        (String::from("key"), api_key),
        (String::from("cx"), engine_id.clone()),
        (String::from("q"), query.clone()),
        (String::from("num"), num.to_string()),
        (String::from("start"), start.to_string()),
        (String::from("safe"), safe),
    ];
    push_optional(arguments, &mut parameters, "search_type", "searchType");
    push_optional(arguments, &mut parameters, "site_search", "siteSearch");
    push_optional(
        arguments,
        &mut parameters,
        "site_search_filter",
        "siteSearchFilter",
    );
    push_optional(arguments, &mut parameters, "date_restrict", "dateRestrict");
    push_optional(arguments, &mut parameters, "file_type", "fileType");
    push_optional(arguments, &mut parameters, "language", "lr");
    push_optional(arguments, &mut parameters, "country", "cr");

    let result = request("search_custom", &parameters).await?;
    let search_info = result.get("searchInformation").and_then(Value::as_object);
    let total_results = search_info
        .and_then(|info| info.get("totalResults"))
        .and_then(Value::as_str)
        .unwrap_or("0");
    let search_time = search_info
        .and_then(|info| info.get("searchTime"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let final_index = start + i64::try_from(items.len()).unwrap_or(i64::MAX) - 1;
    let mut output = format!(
        "Search Results for {user_email}:\n- Query: \"{query}\"\n- Search Engine ID: {engine_id}\n- Total Results: {total_results}\n- Search Time: {search_time:.3} seconds\n- Results Returned: {} (showing {start} to {final_index})\n\n",
        items.len()
    );

    if items.is_empty() {
        output.push_str("\nNo results found.");
    } else {
        output.push_str("Results:\n");
        for (offset, item) in items.iter().enumerate() {
            let result_number = start + i64::try_from(offset).unwrap_or(i64::MAX);
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("No title");
            let link = item
                .get("link")
                .and_then(Value::as_str)
                .unwrap_or("No link");
            let snippet = item
                .get("snippet")
                .and_then(Value::as_str)
                .unwrap_or("No description available")
                .replace('\n', " ");
            output.push_str(&format!(
                "\n{result_number}. {title}\n   URL: {link}\n   Snippet: {snippet}\n"
            ));

            if let Some(meta) = item
                .pointer("/pagemap/metatags/0")
                .and_then(Value::as_object)
            {
                if let Some(kind) = meta.get("og:type").and_then(Value::as_str) {
                    output.push_str(&format!("   Type: {kind}\n"));
                }
                if let Some(published) = meta.get("article:published_time").and_then(Value::as_str)
                {
                    let date = published.get(..10).unwrap_or(published);
                    output.push_str(&format!("   Published: {date}\n"));
                }
            }
        }
    }

    if let Some(next_start) = result
        .pointer("/queries/nextPage/0/startIndex")
        .and_then(Value::as_i64)
    {
        output.push_str(&format!(
            "\n\nTo see more results, search again with start={next_start}"
        ));
    }
    Ok(output)
}

pub async fn get_search_engine_info(arguments: &Map<String, Value>) -> Result<String, String> {
    validate_arguments(arguments, &["user_google_email"])?;
    let user_email = required_user_email(arguments)?;
    let (api_key, engine_id) = credentials()?;
    let parameters = vec![
        (String::from("key"), api_key),
        (String::from("cx"), engine_id.clone()),
        (String::from("q"), String::from("test")),
        (String::from("num"), String::from("1")),
    ];
    let result = request("get_search_engine_info", &parameters).await?;
    let title = result
        .pointer("/context/title")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let mut output = format!(
        "Search Engine Information for {user_email}:\n- Search Engine ID: {engine_id}\n- Title: {title}\n"
    );

    if let Some(facets) = result.pointer("/context/facets").and_then(Value::as_array) {
        output.push_str("\nAvailable Refinements:\n");
        for facet in facets {
            if let Some(entries) = facet.as_array() {
                for entry in entries {
                    let label = entry
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown");
                    let anchor = entry
                        .get("anchor")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown");
                    output.push_str(&format!("  - {label} (anchor: {anchor})\n"));
                }
            }
        }
    }

    if let Some(search_info) = result.get("searchInformation").and_then(Value::as_object) {
        let total_results = search_info
            .get("totalResults")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        output.push_str("\nSearch Statistics:\n");
        output.push_str(&format!("  - Total indexed results: {total_results}\n"));
    }
    Ok(output)
}

async fn request(tool_name: &str, parameters: &[(String, String)]) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| format!("API error in {tool_name}: failed to initialize HTTP client"))?;
    let response = client
        .get(SEARCH_ENDPOINT)
        .query(parameters)
        .send()
        .await
        .map_err(|_| format!("API error in {tool_name}: request to Google Custom Search failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "API error in {tool_name}: Google Custom Search API returned {status}"
        ));
    }
    response
        .json()
        .await
        .map_err(|_| format!("API error in {tool_name}: invalid JSON response"))
}

fn credentials() -> Result<(String, String), String> {
    let api_key = env::var("GOOGLE_PSE_API_KEY").map_err(|_| {
        String::from(
            "GOOGLE_PSE_API_KEY environment variable not set. Please set it to your Google Custom Search API key.",
        )
    })?;
    let engine_id = env::var("GOOGLE_PSE_ENGINE_ID").map_err(|_| {
        String::from(
            "GOOGLE_PSE_ENGINE_ID environment variable not set. Please set it to your Programmable Search Engine ID.",
        )
    })?;
    Ok((api_key, engine_id))
}

fn required_user_email(arguments: &Map<String, Value>) -> Result<String, String> {
    let principal = auth::request_principal_email()
        .or_else(|| env::var("USER_GOOGLE_EMAIL").ok())
        .ok_or_else(|| {
            String::from(
                "Custom Search requires an authenticated request principal or USER_GOOGLE_EMAIL",
            )
        })?;
    let principal = auth::canonical_email(&principal)?;
    if let Some(supplied) = string(arguments, "user_google_email") {
        let supplied = auth::canonical_email(&supplied)?;
        if supplied != principal {
            return Err(String::from(
                "user_google_email does not match the authenticated principal",
            ));
        }
    }
    Ok(principal)
}

fn required_string(arguments: &Map<String, Value>, name: &str) -> Result<String, String> {
    string(arguments, name).ok_or_else(|| format!("{name} is required"))
}

fn string(arguments: &Map<String, Value>, name: &str) -> Option<String> {
    arguments
        .get(name)
        .or_else(|| arguments.get(&snake_to_camel(name)))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn integer(arguments: &Map<String, Value>, name: &str, default: i64) -> Result<i64, String> {
    match arguments
        .get(name)
        .or_else(|| arguments.get(&snake_to_camel(name)))
    {
        None => Ok(default),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| format!("{name} must be an integer")),
    }
}

fn string_list(arguments: &Map<String, Value>, name: &str) -> Result<Option<Vec<String>>, String> {
    let Some(value) = arguments
        .get(name)
        .or_else(|| arguments.get(&snake_to_camel(name)))
    else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let parsed;
    let value = if let Some(encoded) = value.as_str() {
        parsed = serde_json::from_str::<Value>(encoded)
            .map_err(|error| format!("{name} must be an array of strings: {error}"))?;
        &parsed
    } else {
        value
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array of strings"))?;
    values
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} must contain only strings"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
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
            "num" | "start" if !value.is_i64() => {
                return Err(format!("{canonical} must be an integer"));
            }
            "sites" if !(value.is_array() || value.is_string()) => {
                return Err(String::from("sites must be an array of strings"));
            }
            "sites" => {}
            _ if !value.is_string() => {
                return Err(format!("{canonical} must be a string"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_optional_enum(
    arguments: &Map<String, Value>,
    name: &str,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(value) = string(arguments, name)
        && !allowed.contains(&value.as_str())
    {
        return Err(format!("{name} must be one of: {}", allowed.join(", ")));
    }
    Ok(())
}

fn push_optional(
    arguments: &Map<String, Value>,
    parameters: &mut Vec<(String, String)>,
    argument_name: &str,
    api_name: &str,
) {
    if let Some(value) = string(arguments, argument_name) {
        parameters.push((String::from(api_name), value));
    }
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
