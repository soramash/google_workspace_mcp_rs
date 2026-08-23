use std::cmp::Reverse;
use std::env;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::auth::GoogleSession;

const SHEETS_API: &str = "https://sheets.googleapis.com/v4/spreadsheets";
const DRIVE_FILES_API: &str = "https://www.googleapis.com/drive/v3/files";
const SHEETS_READ: &str = "https://www.googleapis.com/auth/spreadsheets.readonly";
const SHEETS_WRITE: &str = "https://www.googleapis.com/auth/spreadsheets";
const DRIVE_READ: &str = "https://www.googleapis.com/auth/drive.readonly";
const DRIVE_FULL: &str = "https://www.googleapis.com/auth/drive";
const MAX_READ_ROWS: i64 = 1000;
const MAX_GRID_METADATA_CELLS: usize = 5000;
const COMMENT_FIELDS: &str = "nextPageToken,comments(id,content,author,createdTime,modifiedTime,resolved,quotedFileContent,replies(content,author,id,createdTime,modifiedTime))";
const COMMENT_RESULT_FIELDS: &str = "id,content,author,createdTime,modifiedTime";
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Clone, Copy)]
enum Kind {
    String,
    Boolean,
    Integer,
    NullableString,
    NullableBoolean,
    NullableInteger,
    NullableStringArray,
    NullableObject,
    NullableIntegerArray,
    NullableStringValues,
    AnyValues,
    ConditionValues,
    GradientPoints,
}

const COMMON_ID: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
];
const CREATE_SPREADSHEET: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("title", Kind::String),
    ("sheet_names", Kind::NullableStringArray),
];
const READ_VALUES: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("range_name", Kind::String),
    ("include_hyperlinks", Kind::Boolean),
    ("include_notes", Kind::Boolean),
    ("include_formulas", Kind::Boolean),
];
const MODIFY_VALUES: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("range_name", Kind::String),
    ("values", Kind::NullableStringValues),
    ("clear_values", Kind::Boolean),
    ("allow_formulas", Kind::Boolean),
];
const LIST_SPREADSHEETS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("max_results", Kind::Integer),
];
const FORMAT_RANGE: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("range_name", Kind::String),
    ("background_color", Kind::NullableString),
    ("text_color", Kind::NullableString),
    ("number_format_type", Kind::NullableString),
    ("number_format_pattern", Kind::NullableString),
    ("wrap_strategy", Kind::NullableString),
    ("horizontal_alignment", Kind::NullableString),
    ("vertical_alignment", Kind::NullableString),
    ("bold", Kind::NullableBoolean),
    ("italic", Kind::NullableBoolean),
    ("font_size", Kind::NullableInteger),
];
const CREATE_SHEET: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("sheet_name", Kind::NullableString),
    ("source_sheet_name", Kind::NullableString),
    ("insert_sheet_index", Kind::NullableInteger),
];
const APPEND_ROWS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("table_id", Kind::String),
    ("values", Kind::AnyValues),
    ("allow_formulas", Kind::Boolean),
];
const RESIZE: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("sheet_name", Kind::NullableString),
    ("column_sizes", Kind::NullableObject),
    ("row_sizes", Kind::NullableObject),
    ("auto_resize_columns", Kind::NullableStringArray),
    ("auto_resize_rows", Kind::NullableIntegerArray),
    ("frozen_row_count", Kind::NullableInteger),
    ("frozen_column_count", Kind::NullableInteger),
    ("hide_columns", Kind::NullableStringArray),
    ("unhide_columns", Kind::NullableStringArray),
    ("hide_rows", Kind::NullableIntegerArray),
    ("unhide_rows", Kind::NullableIntegerArray),
    ("insert_rows", Kind::NullableInteger),
    ("insert_rows_at", Kind::NullableInteger),
    ("insert_columns", Kind::NullableInteger),
    ("insert_columns_at", Kind::NullableString),
    ("delete_rows", Kind::NullableIntegerArray),
    ("delete_row_range", Kind::NullableString),
    ("delete_columns", Kind::NullableStringArray),
];
const MOVE_ROWS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("source_sheet", Kind::String),
    ("start_row", Kind::Integer),
    ("end_row", Kind::Integer),
    ("destination_sheet", Kind::String),
];
const LIST_COMMENTS: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("max_comments", Kind::NullableInteger),
];
const MANAGE_COMMENT: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("action", Kind::String),
    ("comment_content", Kind::NullableString),
    ("comment_id", Kind::NullableString),
];
const CONDITIONAL: &[(&str, Kind)] = &[
    ("user_google_email", Kind::String),
    ("spreadsheet_id", Kind::String),
    ("action", Kind::String),
    ("range_name", Kind::NullableString),
    ("condition_type", Kind::NullableString),
    ("condition_values", Kind::ConditionValues),
    ("background_color", Kind::NullableString),
    ("text_color", Kind::NullableString),
    ("rule_index", Kind::NullableInteger),
    ("gradient_points", Kind::GradientPoints),
    ("sheet_name", Kind::NullableString),
];
const CONDITION_TYPES: &[&str] = &[
    "NUMBER_GREATER",
    "NUMBER_GREATER_THAN_EQ",
    "NUMBER_LESS",
    "NUMBER_LESS_THAN_EQ",
    "NUMBER_EQ",
    "NUMBER_NOT_EQ",
    "TEXT_CONTAINS",
    "TEXT_NOT_CONTAINS",
    "TEXT_STARTS_WITH",
    "TEXT_ENDS_WITH",
    "TEXT_EQ",
    "DATE_BEFORE",
    "DATE_ON_OR_BEFORE",
    "DATE_AFTER",
    "DATE_ON_OR_AFTER",
    "DATE_EQ",
    "DATE_NOT_EQ",
    "DATE_BETWEEN",
    "DATE_NOT_BETWEEN",
    "NOT_BLANK",
    "BLANK",
    "CUSTOM_FORMULA",
    "ONE_OF_RANGE",
];

pub async fn dispatch(tool_name: &str, arguments: &Map<String, Value>) -> Result<String, String> {
    match tool_name {
        "create_spreadsheet" => create_spreadsheet(arguments).await,
        "read_sheet_values" => read_sheet_values(arguments).await,
        "modify_sheet_values" => modify_sheet_values(arguments).await,
        "list_spreadsheets" => list_spreadsheets(arguments).await,
        "get_spreadsheet_info" => get_spreadsheet_info(arguments).await,
        "format_sheet_range" => format_sheet_range(arguments).await,
        "list_sheet_tables" => list_sheet_tables(arguments).await,
        "create_sheet" => create_sheet(arguments).await,
        "append_table_rows" => append_table_rows(arguments).await,
        "resize_sheet_dimensions" => resize_sheet_dimensions(arguments).await,
        "move_sheet_rows" => move_sheet_rows(arguments).await,
        "list_spreadsheet_comments" => list_spreadsheet_comments(arguments).await,
        "manage_spreadsheet_comment" => manage_spreadsheet_comment(arguments).await,
        "manage_conditional_formatting" => manage_conditional_formatting(arguments).await,
        _ => Err(format!("native Rust tool '{tool_name}' is not implemented")),
    }
}

async fn create_spreadsheet(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, CREATE_SPREADSHEET)?;
    let title = required_string(&a, "title")?;
    let names = nullable_string_array(&a, "sheet_names")?;
    let session = auth(&a, SHEETS_WRITE).await?;
    let mut body = json!({"properties": {"title": title}});
    if let Some(names) = names.filter(|v| !v.is_empty()) {
        body["sheets"] = Value::Array(
            names
                .into_iter()
                .map(|name| json!({"properties":{"title":name}}))
                .collect(),
        );
    }
    let result = session
        .send_json(
            "create_spreadsheet",
            session
                .request(Method::POST, SHEETS_API)
                .query(&[(
                    "fields",
                    "spreadsheetId,spreadsheetUrl,properties(title,locale)",
                )])
                .json(&body),
        )
        .await?;
    Ok(format!(
        "Successfully created spreadsheet '{}' for {}. ID: {} | URL: {} | Locale: {}",
        title,
        session.user_email,
        display_field(&result, "spreadsheetId", "None"),
        display_field(&result, "spreadsheetUrl", "None"),
        result
            .pointer("/properties/locale")
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
    ))
}

async fn list_spreadsheets(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, LIST_SPREADSHEETS)?;
    let max = integer_or(&a, "max_results", 25)?;
    if !(1..=1000).contains(&max) {
        return Err("max_results must be between 1 and 1000".into());
    }
    let session = auth(&a, DRIVE_READ).await?;
    let result = session
        .send_json(
            "list_spreadsheets",
            session.request(Method::GET, DRIVE_FILES_API).query(&[
                (
                    "q",
                    "mimeType='application/vnd.google-apps.spreadsheet'".to_string(),
                ),
                ("pageSize", max.to_string()),
                (
                    "fields",
                    "files(id,name,modifiedTime,webViewLink)".to_string(),
                ),
                ("orderBy", "modifiedTime desc".to_string()),
                ("supportsAllDrives", "true".to_string()),
                ("includeItemsFromAllDrives", "true".to_string()),
            ]),
        )
        .await?;
    let files = array(&result, "files");
    if files.is_empty() {
        return Ok(format!("No spreadsheets found for {}.", session.user_email));
    }
    let lines = files
        .iter()
        .map(|file| {
            format!(
                "- \"{}\" (ID: {}) | Modified: {} | Link: {}",
                display_field(file, "name", "Unknown"),
                display_field(file, "id", "Unknown"),
                display_field(file, "modifiedTime", "Unknown"),
                display_field(file, "webViewLink", "No link")
            )
        })
        .collect::<Vec<_>>();
    Ok(format!(
        "Successfully listed {} spreadsheets for {}:\n{}",
        lines.len(),
        session.user_email,
        lines.join("\n")
    ))
}

async fn get_spreadsheet_info(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, COMMON_ID)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let session = auth(&a, SHEETS_READ).await?;
    let result = spreadsheet_get(&session, "get_spreadsheet_info", &id, "spreadsheetId,properties(title,locale),sheets(properties(title,sheetId,gridProperties(rowCount,columnCount)),conditionalFormats)", &[]).await?;
    let title = result
        .pointer("/properties/title")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let locale = result
        .pointer("/properties/locale")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let sheets = array(&result, "sheets");
    let titles = sheet_title_map(sheets);
    let mut lines = Vec::new();
    for sheet in sheets {
        let p = sheet.get("properties").unwrap_or(&Value::Null);
        let name = p.get("title").and_then(Value::as_str).unwrap_or("Unknown");
        let sid = p
            .get("sheetId")
            .map(python_repr)
            .unwrap_or_else(|| "Unknown".into());
        let rows = p
            .pointer("/gridProperties/rowCount")
            .map(python_repr)
            .unwrap_or_else(|| "Unknown".into());
        let cols = p
            .pointer("/gridProperties/columnCount")
            .map(python_repr)
            .unwrap_or_else(|| "Unknown".into());
        let rules = array(sheet, "conditionalFormats");
        lines.push(format!(
            "  - \"{name}\" (ID: {sid}) | Size: {rows}x{cols} | Conditional formats: {}",
            rules.len()
        ));
        if !rules.is_empty() {
            lines.push(format_rules(name, rules, &titles, "    "));
        }
    }
    Ok(format!(
        "Spreadsheet: \"{title}\" (ID: {id}) | Locale: {locale}\nSheets ({}):\n{}",
        sheets.len(),
        if lines.is_empty() {
            "  No sheets found".into()
        } else {
            lines.join("\n")
        }
    ))
}

async fn read_sheet_values(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, READ_VALUES)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let requested = string_or(&a, "range_name", "A1:Z1000")?;
    let hyperlinks = boolean_or(&a, "include_hyperlinks", false)?;
    let notes = boolean_or(&a, "include_notes", false)?;
    let formulas = boolean_or(&a, "include_formulas", false)?;
    let (fetch_range, clamp_note) = clamp_read_range(&requested)?;
    let session = auth(&a, SHEETS_READ).await?;
    let result = values_get(&session, "read_sheet_values", &id, &fetch_range, &[]).await?;
    let values = array(&result, "values").to_vec();
    let resolved = result
        .get("range")
        .and_then(Value::as_str)
        .unwrap_or(&requested)
        .to_string();
    let mut formula_values = Vec::new();
    let mut formula_section = String::new();
    if formulas
        && let Ok(r) = values_get(
            &session,
            "read_sheet_values",
            &id,
            &resolved,
            &[("valueRenderOption", "FORMULA")],
        )
        .await
    {
        formula_values = array(&r, "values").to_vec();
        formula_section = format_formulas(&resolved, &formula_values);
    }
    if values.is_empty() && formula_values.is_empty() {
        return Ok(format!(
            "No data found in range '{}' for {}.{}",
            requested, session.user_email, clamp_note
        ));
    }
    if values.is_empty() {
        return Ok(format!(
            "No displayed values found in range '{}' in spreadsheet {} for {}. The range contains formula cells.{}{}",
            requested, id, session.user_email, formula_section, clamp_note
        ));
    }
    let width = values.first().and_then(Value::as_array).map_or(0, Vec::len);
    let rows = values
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut cells = row.as_array().cloned().unwrap_or_default();
            cells.resize(width.max(cells.len()), Value::String(String::new()));
            format!("Row {:2}: {}", i + 1, python_repr(&Value::Array(cells)))
        })
        .collect::<Vec<_>>();
    let mut output = format!(
        "Successfully read {} rows from range '{}' in spreadsheet {} for {}:\n{}{}",
        values.len(),
        requested,
        id,
        session.user_email,
        rows.join("\n"),
        clamp_note
    );
    if (hyperlinks || notes)
        && let Some(tight) = tight_range(&resolved, &values)
    {
        let row_count = values.len();
        let column_count = values
            .iter()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .max()
            .unwrap_or(0);
        let within_limit = row_count
            .checked_mul(column_count)
            .is_some_and(|area| area <= MAX_GRID_METADATA_CELLS);
        if within_limit
            && let Ok(grid) = fetch_grid_metadata(&session, &id, &tight, hyperlinks, notes).await
        {
            if hyperlinks {
                output.push_str(&format_hyperlinks(&grid, &tight));
            }
            if notes {
                output.push_str(&format_notes(&grid, &tight));
            }
        }
    }
    output.push_str(&formula_section);
    if values_contain_errors(&values)
        && let Some(tight) = tight_range(&resolved, &values)
        && let Ok(grid) = fetch_error_grid(&session, &id, &tight).await
    {
        output.push_str(&format_errors(&grid, &tight));
    }
    Ok(output)
}

async fn modify_sheet_values(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, MODIFY_VALUES)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let range = required_string(&a, "range_name")?;
    let clear = boolean_or(&a, "clear_values", false)?;
    let allow_formulas = boolean_or(&a, "allow_formulas", false)?;
    let values = nullable_values(&a, "values", false)?;
    if !clear && values.as_ref().is_none_or(Vec::is_empty) {
        return Err("Either 'values' must be provided or 'clear_values' must be True.".into());
    }
    let session = auth(&a, SHEETS_WRITE).await?;
    if clear {
        let url = format!("{SHEETS_API}/{}/values/{}:clear", path(&id), path(&range));
        let result = session
            .send_json(
                "modify_sheet_values",
                session.request(Method::POST, &url).json(&json!({})),
            )
            .await?;
        let cleared = result
            .get("clearedRange")
            .and_then(Value::as_str)
            .unwrap_or(&range);
        return Ok(format!(
            "Successfully cleared range '{cleared}' in spreadsheet {id} for {}.",
            session.user_email
        ));
    }
    let url = format!("{SHEETS_API}/{}/values/{}", path(&id), path(&range));
    let result = session
        .send_json(
            "modify_sheet_values",
            session
                .request(Method::PUT, &url)
                .query(&[
                    (
                        "valueInputOption",
                        if allow_formulas {
                            "USER_ENTERED"
                        } else {
                            "RAW"
                        },
                    ),
                    ("includeValuesInResponse", "true"),
                    ("responseValueRenderOption", "FORMATTED_VALUE"),
                ])
                .json(&json!({"values": values.unwrap_or_default()})),
        )
        .await?;
    let mut output = format!(
        "Successfully updated range '{}' in spreadsheet {} for {}. Updated: {} cells, {} rows, {} columns.",
        range,
        id,
        session.user_email,
        int_field(&result, "updatedCells"),
        int_field(&result, "updatedRows"),
        int_field(&result, "updatedColumns")
    );
    let updated = result
        .pointer("/updatedData/values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if values_contain_errors(&updated) {
        let updated_range = result
            .get("updatedRange")
            .and_then(Value::as_str)
            .unwrap_or(&range);
        if let Some(tight) = tight_range(updated_range, &updated)
            && let Ok(grid) = fetch_error_grid(&session, &id, &tight).await
        {
            output.push_str(&format_errors(&grid, &tight));
        }
    }
    Ok(output)
}

async fn format_sheet_range(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, FORMAT_RANGE)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let range = required_string(&a, "range_name")?;
    let bg_raw = nullable_string(&a, "background_color")?;
    let fg_raw = nullable_string(&a, "text_color")?;
    let number_type = nullable_string(&a, "number_format_type")?;
    let pattern = nullable_string(&a, "number_format_pattern")?;
    let wrap = normalized_enum(&a, "wrap_strategy", &["WRAP", "CLIP", "OVERFLOW_CELL"])?;
    let horizontal = normalized_enum(&a, "horizontal_alignment", &["LEFT", "CENTER", "RIGHT"])?;
    let vertical = normalized_enum(&a, "vertical_alignment", &["TOP", "MIDDLE", "BOTTOM"])?;
    let bold = nullable_boolean(&a, "bold")?;
    let italic = nullable_boolean(&a, "italic")?;
    let font_size = nullable_integer(&a, "font_size")?;
    if bg_raw.is_none()
        && fg_raw.is_none()
        && number_type.is_none()
        && wrap.is_none()
        && horizontal.is_none()
        && vertical.is_none()
        && bold.is_none()
        && italic.is_none()
        && font_size.is_none()
    {
        return Err("Provide at least one formatting option (background_color, text_color, number_format_type, wrap_strategy, horizontal_alignment, vertical_alignment, bold, italic, or font_size).".into());
    }
    if font_size.is_some_and(|v| v <= 0) {
        return Err("font_size must be a positive integer.".into());
    }
    let bg = bg_raw.as_deref().map(parse_color).transpose()?;
    let fg = fg_raw.as_deref().map(parse_color).transpose()?;
    let number = if let Some(kind) = number_type.as_deref() {
        let kind = kind.to_ascii_uppercase();
        let allowed = [
            "NUMBER",
            "NUMBER_WITH_GROUPING",
            "CURRENCY",
            "PERCENT",
            "SCIENTIFIC",
            "DATE",
            "TIME",
            "DATE_TIME",
            "TEXT",
        ];
        if !allowed.contains(&kind.as_str()) {
            return Err(format!("number_format_type must be one of {:?}.", allowed));
        }
        let mut n = json!({"type": kind});
        if let Some(p) = pattern.as_deref() {
            n["pattern"] = json!(p);
        }
        Some(n)
    } else {
        None
    };
    let session = auth(&a, SHEETS_WRITE).await?;
    let sheets = fetch_sheets(&session, "format_sheet_range", &id, false, false).await?;
    let grid = parse_grid_range(&range, &sheets)?;
    let mut format = Map::new();
    let mut fields = Vec::new();
    let mut summary = Vec::new();
    if let Some(c) = bg {
        format.insert("backgroundColor".into(), c);
        fields.push("userEnteredFormat.backgroundColor");
        summary.push(format!("background {}", bg_raw.unwrap()));
    }
    let mut text = Map::new();
    if let Some(c) = fg {
        text.insert("foregroundColor".into(), c);
        fields.push("userEnteredFormat.textFormat.foregroundColor");
        summary.push(format!("text color {}", fg_raw.unwrap()));
    }
    if let Some(v) = bold {
        text.insert("bold".into(), json!(v));
        fields.push("userEnteredFormat.textFormat.bold");
        summary.push(if v { "bold".into() } else { "not bold".into() });
    }
    if let Some(v) = italic {
        text.insert("italic".into(), json!(v));
        fields.push("userEnteredFormat.textFormat.italic");
        summary.push(if v {
            "italic".into()
        } else {
            "not italic".into()
        });
    }
    if let Some(v) = font_size {
        text.insert("fontSize".into(), json!(v));
        fields.push("userEnteredFormat.textFormat.fontSize");
        summary.push(format!("font size {v}"));
    }
    if !text.is_empty() {
        format.insert("textFormat".into(), Value::Object(text));
    }
    if let Some(n) = number {
        summary.push(format!(
            "number format {}{}",
            n["type"].as_str().unwrap_or(""),
            pattern
                .as_deref()
                .map(|p| format!(" (pattern: {p})"))
                .unwrap_or_default()
        ));
        format.insert("numberFormat".into(), n);
        fields.push("userEnteredFormat.numberFormat");
    }
    if let Some(v) = wrap {
        format.insert("wrapStrategy".into(), json!(v));
        fields.push("userEnteredFormat.wrapStrategy");
        summary.push(format!("wrap {v}"));
    }
    if let Some(v) = horizontal {
        format.insert("horizontalAlignment".into(), json!(v));
        fields.push("userEnteredFormat.horizontalAlignment");
        summary.push(format!("horizontal align {v}"));
    }
    if let Some(v) = vertical {
        format.insert("verticalAlignment".into(), json!(v));
        fields.push("userEnteredFormat.verticalAlignment");
        summary.push(format!("vertical align {v}"));
    }
    batch_update(&session,"format_sheet_range",&id,vec![json!({"repeatCell":{"range":grid,"cell":{"userEnteredFormat":format},"fields":fields.join(",")}})]).await?;
    Ok(format!(
        "Applied formatting to range '{}' in spreadsheet {} for {}: {}.",
        range,
        id,
        session.user_email,
        summary.join(", ")
    ))
}

async fn create_sheet(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, CREATE_SHEET)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let name = nullable_string(&a, "sheet_name")?;
    let source = nullable_string(&a, "source_sheet_name")?;
    let index = nullable_integer(&a, "insert_sheet_index")?;
    if index.is_some_and(|v| v < 0) {
        return Err("insert_sheet_index must be a non-negative integer.".into());
    }
    let session = auth(&a, SHEETS_WRITE).await?;
    let request = if let Some(source) = source.as_deref() {
        if source.trim().is_empty() {
            return Err("source_sheet_name must be a non-empty string".into());
        }
        let sheets = fetch_sheets(&session, "create_sheet", &id, false, false).await?;
        let sid = sheet_props(select_sheet(&sheets, Some(source))?)
            .get("sheetId")
            .and_then(Value::as_i64)
            .ok_or("API response missing sheetId")?;
        let mut dup = json!({"sourceSheetId":sid});
        if let Some(ref n) = name {
            dup["newSheetName"] = json!(n);
        }
        if let Some(i) = index {
            dup["insertSheetIndex"] = json!(i);
        }
        json!({"duplicateSheet":dup})
    } else {
        let mut props = Map::new();
        if let Some(ref n) = name {
            props.insert("title".into(), json!(n));
        }
        if let Some(i) = index {
            props.insert("index".into(), json!(i));
        }
        json!({"addSheet":{"properties":props}})
    };
    let result = batch_update(&session, "create_sheet", &id, vec![request]).await?;
    let props = if source.is_some() {
        result.pointer("/replies/0/duplicateSheet/properties")
    } else {
        result.pointer("/replies/0/addSheet/properties")
    }
    .ok_or("API response missing created sheet properties")?;
    let created = props
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(name.as_deref().unwrap_or("Untitled"));
    let sid = props
        .get("sheetId")
        .map(python_repr)
        .unwrap_or_else(|| "Unknown".into());
    Ok(if let Some(source) = source {
        format!(
            "Successfully duplicated '{}' to '{}' (ID: {}) in spreadsheet {} for {}.",
            source, created, sid, id, session.user_email
        )
    } else {
        format!(
            "Successfully created sheet '{}' (ID: {}) in spreadsheet {} for {}.",
            created, sid, id, session.user_email
        )
    })
}

async fn list_sheet_tables(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, COMMON_ID)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let session = auth(&a, SHEETS_READ).await?;
    let result = spreadsheet_get(
        &session,
        "list_sheet_tables",
        &id,
        "sheets(properties(title,sheetId),tables)",
        &[],
    )
    .await?;
    let mut found = Vec::new();
    for sheet in array(&result, "sheets") {
        let title = sheet
            .pointer("/properties/title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        for table in array(sheet, "tables") {
            let range = table.get("range").unwrap_or(&Value::Null);
            let columns = array(table, "columnProperties")
                .iter()
                .filter_map(|v| v.get("columnName").and_then(Value::as_str))
                .collect::<Vec<_>>();
            found.push(format!("  Table ID: {}\n  Name: {}\n  Sheet: {}\n  Range: rows {}-{}, cols {}-{}\n  Columns: {}", display_field(table,"tableId","None"),display_field(table,"name","Unnamed"),title, range.get("startRowIndex").map(python_repr).unwrap_or_else(||"0".into()),range.get("endRowIndex").map(python_repr).unwrap_or_else(||"?".into()), range.get("startColumnIndex").map(python_repr).unwrap_or_else(||"0".into()),range.get("endColumnIndex").map(python_repr).unwrap_or_else(||"?".into()),if columns.is_empty(){"N/A".into()}else{columns.join(", ")}));
        }
    }
    if found.is_empty() {
        Ok(format!(
            "No structured tables found in spreadsheet {} for {}.",
            id, session.user_email
        ))
    } else {
        Ok(format!(
            "Found {} table(s) in spreadsheet {} for {}:\n\n{}",
            found.len(),
            id,
            session.user_email,
            found.join("\n\n")
        ))
    }
}

async fn append_table_rows(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, APPEND_ROWS)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let table = required_string(&a, "table_id")?;
    let values = nullable_values(&a, "values", true)?
        .ok_or("values must be a non-empty 2D list of cell values.")?;
    if values.is_empty() {
        return Err("values must be a non-empty 2D list of cell values.".into());
    }
    let formulas = boolean_or(&a, "allow_formulas", false)?;
    let session = auth(&a, SHEETS_WRITE).await?;
    let metadata = spreadsheet_get(
        &session,
        "append_table_rows",
        &id,
        "sheets(properties(sheetId),tables(tableId))",
        &[],
    )
    .await?;
    let mut sheet_id = None;
    for sheet in array(&metadata, "sheets") {
        if array(sheet, "tables")
            .iter()
            .any(|t| t.get("tableId").and_then(Value::as_str) == Some(&table))
        {
            sheet_id = sheet.pointer("/properties/sheetId").and_then(Value::as_i64);
            break;
        }
    }
    let sid=sheet_id.ok_or_else(||format!("Table '{}' not found in spreadsheet {}. Use list_sheet_tables to find valid table IDs.",table,id))?;
    let rows=values.iter().map(|row|json!({"values":row.iter().map(|v|json!({"userEnteredValue":extended_value(v,formulas)})).collect::<Vec<_>>() })).collect::<Vec<_>>();
    batch_update(&session,"append_table_rows",&id,vec![json!({"appendCells":{"sheetId":sid,"tableId":table,"rows":rows,"fields":"userEnteredValue"}})]).await?;
    Ok(format!(
        "Successfully appended {} row(s) to table '{}' in spreadsheet {} for {}.",
        values.len(),
        table,
        id,
        session.user_email
    ))
}

async fn resize_sheet_dimensions(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, RESIZE)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let operation_names = [
        "column_sizes",
        "row_sizes",
        "auto_resize_columns",
        "auto_resize_rows",
        "frozen_row_count",
        "frozen_column_count",
        "hide_columns",
        "unhide_columns",
        "hide_rows",
        "unhide_rows",
        "insert_rows",
        "insert_columns",
        "delete_rows",
        "delete_row_range",
        "delete_columns",
    ];
    if !operation_names
        .iter()
        .any(|n| a.get(*n).is_some_and(|v| !v.is_null()))
    {
        return Err("Provide at least one dimension operation.".into());
    }
    let session = auth(&a, SHEETS_WRITE).await?;
    let sheets = fetch_sheets(&session, "resize_sheet_dimensions", &id, false, false).await?;
    let selected = select_sheet(&sheets, nullable_string(&a, "sheet_name")?.as_deref())?;
    let sid = sheet_props(selected)
        .get("sheetId")
        .and_then(Value::as_i64)
        .ok_or("API response missing sheetId")?;
    let mut requests = Vec::new();
    let mut summary = Vec::new();
    if let Some(map) = nullable_object(&a, "column_sizes")? {
        for (letter, size) in &map {
            let col = column_index(letter)?;
            let px = positive_number(size, &format!("Pixel size for column '{letter}'"))?;
            requests.push(dimension_property(
                sid,
                "COLUMNS",
                col,
                col + 1,
                "pixelSize",
                json!(px),
            ));
        }
        summary.push(format!(
            "resized columns: {}",
            map.iter()
                .map(|(k, v)| format!("{}={}px", k, python_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(map) = nullable_object(&a, "row_sizes")? {
        for (row, size) in &map {
            let row: i64 = row
                .parse()
                .map_err(|_| format!("Row number must be an integer >= 1, got {row}."))?;
            if row < 1 {
                return Err(format!("Row number must be >= 1, got {row}."));
            }
            let px = positive_number(size, &format!("Pixel size for row {row}"))?;
            requests.push(dimension_property(
                sid,
                "ROWS",
                row - 1,
                row,
                "pixelSize",
                json!(px),
            ));
        }
        summary.push(format!(
            "resized rows: {}",
            map.iter()
                .map(|(k, v)| format!("{}={}px", k, python_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    add_auto_resize(
        &a,
        "auto_resize_columns",
        sid,
        "COLUMNS",
        &mut requests,
        &mut summary,
    )?;
    add_auto_resize(
        &a,
        "auto_resize_rows",
        sid,
        "ROWS",
        &mut requests,
        &mut summary,
    )?;
    let frozen_rows = nullable_integer(&a, "frozen_row_count")?;
    let frozen_cols = nullable_integer(&a, "frozen_column_count")?;
    let mut grid = Map::new();
    let mut fields = Vec::new();
    if let Some(v) = frozen_rows {
        if v < 0 {
            return Err("frozen_row_count must be a non-negative integer.".into());
        }
        grid.insert("frozenRowCount".into(), json!(v));
        fields.push("gridProperties.frozenRowCount");
        summary.push(if v > 0 {
            format!("froze {v} row(s)")
        } else {
            "unfroze rows".into()
        });
    }
    if let Some(v) = frozen_cols {
        if v < 0 {
            return Err("frozen_column_count must be a non-negative integer.".into());
        }
        grid.insert("frozenColumnCount".into(), json!(v));
        fields.push("gridProperties.frozenColumnCount");
        summary.push(if v > 0 {
            format!("froze {v} column(s)")
        } else {
            "unfroze columns".into()
        });
    }
    if !grid.is_empty() {
        requests.push(json!({"updateSheetProperties":{"properties":{"sheetId":sid,"gridProperties":grid},"fields":fields.join(",")}}));
    }
    for (name, dimension, hidden, label) in [
        ("hide_columns", "COLUMNS", true, "hid columns"),
        ("unhide_columns", "COLUMNS", false, "unhid columns"),
        ("hide_rows", "ROWS", true, "hid rows"),
        ("unhide_rows", "ROWS", false, "unhid rows"),
    ] {
        let items: Option<Vec<Value>> = if dimension == "COLUMNS" {
            nullable_string_array(&a, name)?.map(|v| v.into_iter().map(Value::String).collect())
        } else {
            nullable_integer_array(&a, name)?.map(|v| v.into_iter().map(|n| json!(n)).collect())
        };
        if let Some(items) = items {
            for item in &items {
                let index = if dimension == "COLUMNS" {
                    column_index(item.as_str().unwrap_or(""))?
                } else {
                    let n = item.as_i64().unwrap_or(0);
                    if n < 1 {
                        return Err(format!("Row number must be >= 1 in {name}, got {n}."));
                    }
                    n - 1
                };
                requests.push(dimension_property(
                    sid,
                    dimension,
                    index,
                    index + 1,
                    "hiddenByUser",
                    json!(hidden),
                ));
            }
            summary.push(format!(
                "{label}: {}",
                items.iter().map(python_repr).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    add_insert(
        &a,
        sid,
        "ROWS",
        "insert_rows",
        "insert_rows_at",
        &mut requests,
        &mut summary,
    )?;
    add_insert(
        &a,
        sid,
        "COLUMNS",
        "insert_columns",
        "insert_columns_at",
        &mut requests,
        &mut summary,
    )?;
    let delete_rows = nullable_integer_array(&a, "delete_rows")?;
    let delete_range = nullable_string(&a, "delete_row_range")?;
    if delete_rows.is_some() && delete_range.is_some() {
        return Err(
            "delete_rows and delete_row_range cannot be used together. Specify one or the other."
                .into(),
        );
    }
    if let Some(rows) = delete_rows {
        let mut sorted = rows.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        for n in sorted {
            if n < 1 {
                return Err(format!("Row number must be >= 1 in delete_rows, got {n}."));
            }
            requests.push(delete_dimension(sid, "ROWS", n - 1, n));
        }
        summary.push(format!(
            "deleted rows: {}",
            rows.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(spec) = delete_range {
        let (start, end) = parse_row_range(&spec)?;
        requests.push(delete_dimension(sid, "ROWS", start - 1, end));
        summary.push(format!(
            "deleted row range {start}-{end} ({} row(s))",
            end - start + 1
        ));
    }
    if let Some(cols) = nullable_string_array(&a, "delete_columns")? {
        let mut indexed = cols
            .iter()
            .map(|c| Ok((c, column_index(c)?)))
            .collect::<Result<Vec<_>, String>>()?;
        indexed.sort_by_key(|item| Reverse(item.1));
        for (_, i) in indexed {
            requests.push(delete_dimension(sid, "COLUMNS", i, i + 1));
        }
        summary.push(format!("deleted columns: {}", cols.join(", ")));
    }
    batch_update(&session, "resize_sheet_dimensions", &id, requests).await?;
    Ok(format!(
        "Applied dimension changes in spreadsheet {} for {}: {}.",
        id,
        session.user_email,
        summary.join("; ")
    ))
}

async fn move_sheet_rows(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, MOVE_ROWS)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let source = required_string(&a, "source_sheet")?;
    let destination = required_string(&a, "destination_sheet")?;
    let start = required_integer(&a, "start_row")?;
    let end = required_integer(&a, "end_row")?;
    if start < 1 || end < start {
        return Err(format!(
            "Invalid row range: start_row={start}, end_row={end}. Rows are 1-based and end_row must be >= start_row."
        ));
    }
    if source == destination {
        return Err("source_sheet and destination_sheet must be different.".into());
    }
    let session = auth(&a, SHEETS_WRITE).await?;
    let sheets = fetch_sheets(&session, "move_sheet_rows", &id, false, true).await?;
    let src = select_sheet(&sheets, Some(&source))?;
    let dst = select_sheet(&sheets, Some(&destination))?;
    let src_id = sheet_props(src)
        .get("sheetId")
        .and_then(Value::as_i64)
        .ok_or("API response missing source sheetId")?;
    let dst_id = sheet_props(dst)
        .get("sheetId")
        .and_then(Value::as_i64)
        .ok_or("API response missing destination sheetId")?;
    let allocated = dst
        .pointer("/properties/gridProperties/rowCount")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let source_range = format!("{}!{}:{}", quote_sheet_always(&source), start, end);
    let source_values = values_get(&session, "move_sheet_rows", &id, &source_range, &[]).await?;
    if array(&source_values, "values").is_empty() {
        return Err(format!(
            "Source range '{}' rows {}-{} contains no data. Nothing to move.",
            source, start, end
        ));
    }
    let destination_range = quote_sheet_always(&destination);
    let dest_values = values_get(
        &session,
        "move_sheet_rows",
        &id,
        &destination_range,
        &[("majorDimension", "ROWS")],
    )
    .await?;
    let paste = array(&dest_values, "values").len() as i64;
    let count = end - start + 1;
    let mut requests = Vec::new();
    if paste + count > allocated {
        requests.push(json!({"appendDimension":{"sheetId":dst_id,"dimension":"ROWS","length":paste+count-allocated}}));
    }
    requests.push(json!({"copyPaste":{"source":{"sheetId":src_id,"startRowIndex":start-1,"endRowIndex":end},"destination":{"sheetId":dst_id,"startRowIndex":paste,"endRowIndex":paste+count},"pasteType":"PASTE_NORMAL"}}));
    requests.push(delete_dimension(src_id, "ROWS", start - 1, end));
    batch_update(&session, "move_sheet_rows", &id, requests).await?;
    Ok(format!(
        "Successfully moved {} row(s) from '{}' (rows {}-{}) to '{}' in spreadsheet {} for {}.",
        count, source, start, end, destination, id, session.user_email
    ))
}

async fn list_spreadsheet_comments(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, LIST_COMMENTS)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let mut max = nullable_integer(&a, "max_comments")?.unwrap_or_else(|| {
        env::var("WORKSPACE_MCP_COMMENTS_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    });
    if max < 0 {
        max = 100;
    }
    if max == 0 {
        return Ok(format!("No comments found in spreadsheet {id}"));
    }
    let session = auth(&a, DRIVE_READ).await?;
    let mut comments = Vec::new();
    let mut token: Option<String> = None;
    while (comments.len() as i64) < max {
        let page = (max - comments.len() as i64).min(100);
        let url = format!("{DRIVE_FILES_API}/{}/comments", path(&id));
        let mut query = vec![
            ("fields", COMMENT_FIELDS.to_string()),
            ("pageSize", page.to_string()),
        ];
        if let Some(ref t) = token {
            query.push(("pageToken", t.clone()));
        }
        let response = session
            .send_json(
                "list_spreadsheet_comments",
                session.request(Method::GET, &url).query(&query),
            )
            .await?;
        let remaining = (max - comments.len() as i64) as usize;
        comments.extend(array(&response, "comments").iter().take(remaining).cloned());
        token = response
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if token.is_none() {
            break;
        }
    }
    if comments.is_empty() {
        return Ok(format!("No comments found in spreadsheet {id}"));
    }
    let mut out = vec![format!(
        "Found {} comments in spreadsheet {}:\n",
        comments.len(),
        id
    )];
    for c in comments {
        let status = if c.get("resolved").and_then(Value::as_bool).unwrap_or(false) {
            " [RESOLVED]"
        } else {
            ""
        };
        out.push(format!("Comment ID: {}", display_field(&c, "id", "")));
        out.push(format!(
            "Author: {}",
            c.pointer("/author/displayName")
                .and_then(Value::as_str)
                .unwrap_or("Unknown")
        ));
        out.push(format!(
            "Created: {}{}",
            display_field(&c, "createdTime", ""),
            status
        ));
        if let Some(q) = c
            .pointer("/quotedFileContent/value")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            out.push(format!("Quoted text: {q}"));
        }
        out.push(format!("Content: {}", display_field(&c, "content", "")));
        let replies = array(&c, "replies");
        if !replies.is_empty() {
            out.push(format!("  Replies ({}):", replies.len()));
            for r in replies {
                out.push(format!("    Reply ID: {}", display_field(r, "id", "")));
                out.push(format!(
                    "    Author: {}",
                    r.pointer("/author/displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown")
                ));
                out.push(format!(
                    "    Created: {}",
                    display_field(r, "createdTime", "")
                ));
                out.push(format!("    Content: {}", display_field(r, "content", "")));
            }
        }
        out.push(String::new());
    }
    Ok(out.join("\n"))
}

async fn manage_spreadsheet_comment(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, MANAGE_COMMENT)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let action = required_string(&a, "action")?.trim().to_ascii_lowercase();
    let content = nullable_string(&a, "comment_content")?;
    let comment = nullable_string(&a, "comment_id")?;
    let session = auth(&a, DRIVE_FULL).await?;
    let (url, body) = match action.as_str() {
        "create" => {
            let value = content
                .as_deref()
                .filter(|v| !v.is_empty())
                .ok_or("comment_content is required for create action")?;
            (
                format!("{DRIVE_FILES_API}/{}/comments", path(&id)),
                json!({"content":value}),
            )
        }
        "reply" => {
            let cid = comment
                .as_deref()
                .filter(|v| !v.is_empty())
                .ok_or("comment_id and comment_content are required for reply action")?;
            let value = content
                .as_deref()
                .filter(|v| !v.is_empty())
                .ok_or("comment_id and comment_content are required for reply action")?;
            (
                format!(
                    "{DRIVE_FILES_API}/{}/comments/{}/replies",
                    path(&id),
                    path(cid)
                ),
                json!({"content":value}),
            )
        }
        "resolve" => {
            let cid = comment
                .as_deref()
                .filter(|v| !v.is_empty())
                .ok_or("comment_id is required for resolve action")?;
            (
                format!(
                    "{DRIVE_FILES_API}/{}/comments/{}/replies",
                    path(&id),
                    path(cid)
                ),
                json!({"content":"This comment has been resolved.","action":"resolve"}),
            )
        }
        _ => {
            return Err(format!(
                "Invalid action '{}'. Must be 'create', 'reply', or 'resolve'.",
                action
            ));
        }
    };
    let result = session
        .send_json(
            "manage_spreadsheet_comment",
            session
                .request(Method::POST, &url)
                .query(&[("fields", COMMENT_RESULT_FIELDS)])
                .json(&body),
        )
        .await?;
    let author = result
        .pointer("/author/displayName")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let created = display_field(&result, "createdTime", "");
    let rid = display_field(&result, "id", "");
    Ok(match action.as_str() {
        "create" => format!(
            "Comment created successfully!\nComment ID: {rid}\nAuthor: {author}\nCreated: {created}\nContent: {}",
            content.unwrap_or_default()
        ),
        "reply" => format!(
            "Reply posted successfully!\nReply ID: {rid}\nAuthor: {author}\nCreated: {created}\nContent: {}",
            content.unwrap_or_default()
        ),
        _ => format!(
            "Comment {} has been resolved successfully.\nResolve reply ID: {rid}\nAuthor: {author}\nCreated: {created}",
            comment.unwrap_or_default()
        ),
    })
}

async fn manage_conditional_formatting(raw: &Map<String, Value>) -> Result<String, String> {
    let a = normalize(raw, CONDITIONAL)?;
    let id = required_string(&a, "spreadsheet_id")?;
    let action = required_string(&a, "action")?.trim().to_ascii_lowercase();
    if !["add", "update", "delete"].contains(&action.as_str()) {
        return Err(format!(
            "action must be one of [\"add\", \"delete\", \"update\"], got '{}'.",
            action
        ));
    }
    let range = nullable_string(&a, "range_name")?;
    let sheet_name = nullable_string(&a, "sheet_name")?;
    let condition_type = nullable_string(&a, "condition_type")?;
    let condition_values = condition_values(&a)?;
    let gradient = gradient_points(&a)?;
    let background = nullable_string(&a, "background_color")?;
    let text = nullable_string(&a, "text_color")?;
    let index = nullable_integer(&a, "rule_index")?;
    if index.is_some_and(|v| v < 0) {
        return Err("rule_index must be a non-negative integer.".into());
    }
    let session = auth(&a, SHEETS_WRITE).await?;
    let sheets = fetch_sheets(&session, "manage_conditional_formatting", &id, true, false).await?;
    let titles = sheet_title_map(&sheets);
    match action.as_str() {
        "add" => {
            let range = range
                .as_deref()
                .ok_or("range_name is required for action 'add'.")?;
            if condition_type.is_none() && gradient.is_none() {
                return Err(
                    "condition_type (or gradient_points) is required for action 'add'.".into(),
                );
            }
            let grid = parse_grid_range(range, &sheets)?;
            let sid = grid
                .get("sheetId")
                .and_then(Value::as_i64)
                .ok_or("Target sheet not found while adding conditional formatting.")?;
            let sheet = sheets
                .iter()
                .find(|s| sheet_props(s).get("sheetId").and_then(Value::as_i64) == Some(sid))
                .ok_or("Target sheet not found while adding conditional formatting.")?;
            let rules = array(sheet, "conditionalFormats");
            let insert = index.unwrap_or(rules.len() as i64);
            if insert > rules.len() as i64 {
                return Err(format!(
                    "rule_index {} is out of range for sheet '{}' (current count: {}).",
                    insert,
                    sheet_title(sheet),
                    rules.len()
                ));
            }
            let (rule, desc, format_desc) = if let Some(points) = gradient {
                (
                    build_gradient(vec![grid], points.clone()),
                    "gradient".into(),
                    format!("gradient points {}", points.len()),
                )
            } else {
                let (rule, kind) = build_boolean(
                    vec![grid],
                    condition_type.as_deref().unwrap_or(""),
                    condition_values.as_deref(),
                    background.as_deref(),
                    text.as_deref(),
                )?;
                let mut formats = Vec::new();
                if let Some(ref v) = background {
                    formats.push(format!("background {v}"));
                }
                if let Some(ref v) = text {
                    formats.push(format!("text {v}"));
                }
                (
                    rule,
                    kind,
                    if formats.is_empty() {
                        "format applied".into()
                    } else {
                        formats.join(", ")
                    },
                )
            };
            let mut req = json!({"rule":rule});
            if index.is_some() {
                req["index"] = json!(insert);
            }
            batch_update(
                &session,
                "manage_conditional_formatting",
                &id,
                vec![json!({"addConditionalFormatRule":req})],
            )
            .await?;
            let mut state = rules.to_vec();
            state.insert(insert as usize, rule);
            Ok(format!(
                "Added conditional format on '{}' in spreadsheet {} for {}: {}{}; format: {}.\n{}",
                range,
                id,
                session.user_email,
                desc,
                values_desc(condition_values.as_deref()),
                format_desc,
                format_rules(sheet_title(sheet), &state, &titles, "")
            ))
        }
        "delete" => {
            let index = index.ok_or("rule_index is required for action 'delete'.")?;
            let sheet = select_sheet(&sheets, sheet_name.as_deref())?;
            let rules = array(sheet, "conditionalFormats");
            if index as usize >= rules.len() {
                return Err(format!(
                    "rule_index {} is out of range for sheet '{}' (current count: {}).",
                    index,
                    sheet_title(sheet),
                    rules.len()
                ));
            }
            let sid = sheet_props(sheet)
                .get("sheetId")
                .and_then(Value::as_i64)
                .ok_or("API response missing sheetId")?;
            batch_update(
                &session,
                "manage_conditional_formatting",
                &id,
                vec![json!({"deleteConditionalFormatRule":{"index":index,"sheetId":sid}})],
            )
            .await?;
            let mut state = rules.to_vec();
            state.remove(index as usize);
            Ok(format!(
                "Deleted conditional format at index {} on sheet '{}' in spreadsheet {} for {}.\n{}",
                index,
                sheet_title(sheet),
                id,
                session.user_email,
                format_rules(sheet_title(sheet), &state, &titles, "")
            ))
        }
        _ => {
            let index = index.ok_or("rule_index is required for action 'update'.")?;
            let (sheet, new_grid) = if let Some(ref r) = range {
                let grid = parse_grid_range(r, &sheets)?;
                let sid = grid.get("sheetId").and_then(Value::as_i64);
                (
                    sheets
                        .iter()
                        .find(|s| sheet_props(s).get("sheetId").and_then(Value::as_i64) == sid)
                        .ok_or("Target sheet not found while updating conditional formatting.")?,
                    Some(grid),
                )
            } else {
                (select_sheet(&sheets, sheet_name.as_deref())?, None)
            };
            let rules = array(sheet, "conditionalFormats");
            if index as usize >= rules.len() {
                return Err(format!(
                    "rule_index {} is out of range for sheet '{}' (current count: {}).",
                    index,
                    sheet_title(sheet),
                    rules.len()
                ));
            }
            let existing = &rules[index as usize];
            let ranges = new_grid
                .map(|g| vec![g])
                .unwrap_or_else(|| array(existing, "ranges").to_vec());
            let sid = sheet_props(sheet)
                .get("sheetId")
                .and_then(Value::as_i64)
                .ok_or("API response missing sheetId")?;
            let (rule, desc, format_desc) = if let Some(points) = gradient {
                (
                    build_gradient(ranges, points.clone()),
                    "gradient".into(),
                    format!("gradient points {}", points.len()),
                )
            } else if existing.get("gradientRule").is_some() {
                if background.is_some()
                    || text.is_some()
                    || condition_type.is_some()
                    || condition_values.is_some()
                {
                    return Err("Existing rule is a gradient rule. Provide gradient_points to update it, or omit formatting/condition parameters to keep it unchanged.".into());
                }
                (
                    json!({"ranges":ranges,"gradientRule":existing.get("gradientRule").cloned().unwrap_or_else(||json!({}))}),
                    "gradient".into(),
                    "gradient (unchanged)".into(),
                )
            } else {
                let old = existing
                    .get("booleanRule")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let kind = condition_type
                    .clone()
                    .or_else(|| {
                        old.pointer("/condition/type")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .ok_or("condition_type is required for boolean rules.")?
                    .to_ascii_uppercase();
                if !CONDITION_TYPES.contains(&kind.as_str()) {
                    return Err(condition_type_error());
                }
                let vals = condition_values
                    .as_ref()
                    .map(|v| {
                        v.iter()
                            .map(|x| json!({"userEnteredValue":scalar_string(x)}))
                            .collect()
                    })
                    .or_else(|| {
                        old.pointer("/condition/values")
                            .and_then(Value::as_array)
                            .cloned()
                    });
                let mut fmt = old
                    .get("format")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if let Some(ref color) = background {
                    if color.is_empty() {
                        fmt.remove("backgroundColor");
                    } else {
                        fmt.insert("backgroundColor".into(), parse_color(color)?);
                    }
                }
                if let Some(ref color) = text {
                    let mut tf = fmt
                        .get("textFormat")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    if color.is_empty() {
                        tf.remove("foregroundColor");
                    } else {
                        tf.insert("foregroundColor".into(), parse_color(color)?);
                    }
                    if tf.is_empty() {
                        fmt.remove("textFormat");
                    } else {
                        fmt.insert("textFormat".into(), Value::Object(tf));
                    }
                }
                if fmt.is_empty() {
                    return Err("At least one format option must remain on the rule.".into());
                }
                let mut condition = json!({"type":kind});
                if let Some(v) = vals.filter(|v| !v.is_empty()) {
                    condition["values"] = Value::Array(v);
                }
                let desc = kind.clone();
                (
                    json!({"ranges":if ranges.is_empty(){vec![json!({"sheetId":sid})]}else{ranges},"booleanRule":{"condition":condition,"format":fmt}}),
                    desc,
                    "format preserved".into(),
                )
            };
            batch_update(&session,"manage_conditional_formatting",&id,vec![json!({"updateConditionalFormatRule":{"index":index,"sheetId":sid,"rule":rule}})]).await?;
            let mut state = rules.to_vec();
            state[index as usize] = rule;
            Ok(format!(
                "Updated conditional format at index {} on sheet '{}' in spreadsheet {} for {}: {}{}; format: {}.\n{}",
                index,
                sheet_title(sheet),
                id,
                session.user_email,
                desc,
                values_desc(condition_values.as_deref()),
                format_desc,
                format_rules(sheet_title(sheet), &state, &titles, "")
            ))
        }
    }
}

async fn auth(a: &Map<String, Value>, scope: &str) -> Result<GoogleSession, String> {
    let supplied = required_string(a, "user_google_email")?;
    let session = match GoogleSession::authenticate(&[scope]).await {
        Ok(session) => session,
        Err(error)
            if scope == SHEETS_READ
                && error.contains("do not include the required Google scopes") =>
        {
            GoogleSession::authenticate(&[SHEETS_WRITE]).await?
        }
        Err(error) => return Err(error),
    };
    if !supplied.eq_ignore_ascii_case(&session.user_email) {
        return Err("user_google_email does not match the configured USER_GOOGLE_EMAIL".into());
    }
    Ok(session)
}
fn path(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}
async fn spreadsheet_get(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    fields: &str,
    extra: &[(&str, &str)],
) -> Result<Value, String> {
    let url = format!("{SHEETS_API}/{}", path(id));
    let mut q = vec![("fields", fields)];
    q.extend_from_slice(extra);
    session
        .send_json(tool, session.request(Method::GET, &url).query(&q))
        .await
}
async fn values_get(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    range: &str,
    extra: &[(&str, &str)],
) -> Result<Value, String> {
    let url = format!("{SHEETS_API}/{}/values/{}", path(id), path(range));
    session
        .send_json(tool, session.request(Method::GET, &url).query(extra))
        .await
}
async fn batch_update(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    requests: Vec<Value>,
) -> Result<Value, String> {
    if requests.is_empty() {
        return Err("No typed batchUpdate requests were generated.".into());
    }
    let url = format!("{SHEETS_API}/{}:batchUpdate", path(id));
    session
        .send_json(
            tool,
            session
                .request(Method::POST, &url)
                .json(&json!({"requests":requests})),
        )
        .await
}
async fn fetch_sheets(
    session: &GoogleSession,
    tool: &str,
    id: &str,
    rules: bool,
    grid: bool,
) -> Result<Vec<Value>, String> {
    let mut fields = "sheets(properties(sheetId,title".to_string();
    if grid {
        fields.push_str(",gridProperties(rowCount,columnCount)");
    }
    fields.push(')');
    if rules {
        fields.push_str(",conditionalFormats");
    }
    fields.push(')');
    let result = spreadsheet_get(session, tool, id, &fields, &[]).await?;
    Ok(array(&result, "sheets").to_vec())
}

fn normalize(
    raw: &Map<String, Value>,
    schema: &[(&str, Kind)],
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (key, value) in raw {
        let Some((canonical, kind)) = schema
            .iter()
            .find(|(name, _)| key == *name || key == &snake_to_camel(name))
        else {
            return Err(format!("Unexpected argument: {key}"));
        };
        if out.contains_key(*canonical) {
            return Err(format!("Unexpected argument: {key}"));
        }
        validate_kind(value, *kind, canonical)?;
        out.insert((*canonical).into(), value.clone());
    }
    Ok(out)
}
fn validate_kind(value: &Value, kind: Kind, name: &str) -> Result<(), String> {
    let nullable = matches!(
        kind,
        Kind::NullableString
            | Kind::NullableBoolean
            | Kind::NullableInteger
            | Kind::NullableStringArray
            | Kind::NullableObject
            | Kind::NullableIntegerArray
            | Kind::NullableStringValues
            | Kind::ConditionValues
            | Kind::GradientPoints
    );
    if value.is_null() {
        return if nullable {
            Ok(())
        } else {
            Err(format!("{name} must not be null"))
        };
    }
    let valid = match kind {
        Kind::String => value.is_string(),
        Kind::Boolean => value.is_boolean(),
        Kind::Integer => value.as_i64().is_some(),
        Kind::NullableString => value.is_string(),
        Kind::NullableBoolean => value.is_boolean(),
        Kind::NullableInteger => value.as_i64().is_some(),
        Kind::NullableStringArray => container(value, name)?
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string)),
        Kind::NullableObject => container(value, name)?.is_object(),
        Kind::NullableIntegerArray => container(value, name)?
            .as_array()
            .is_some_and(|a| a.iter().all(|v| v.as_i64().is_some())),
        Kind::NullableStringValues => valid_values(&container(value, name)?, false),
        Kind::AnyValues => valid_values(&container(value, name)?, true),
        Kind::ConditionValues => container(value, name)?
            .as_array()
            .is_some_and(|a| a.iter().all(is_scalar_condition)),
        Kind::GradientPoints => container(value, name)?
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_object)),
    };
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid type for argument: {name}"))
    }
}
fn container(value: &Value, name: &str) -> Result<Value, String> {
    if let Some(s) = value.as_str() {
        serde_json::from_str(s).map_err(|e| format!("Invalid JSON for {name}: {e}"))
    } else {
        Ok(value.clone())
    }
}
fn valid_values(value: &Value, any: bool) -> bool {
    value.as_array().is_some_and(|rows| {
        rows.iter().all(|row| {
            row.as_array()
                .is_some_and(|cells| any || cells.iter().all(Value::is_string))
        })
    })
}
fn is_scalar_condition(v: &Value) -> bool {
    v.is_string() || v.is_number()
}
fn snake_to_camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
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
fn get<'a>(a: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    a.get(name)
}
fn required_string(a: &Map<String, Value>, name: &str) -> Result<String, String> {
    get(a, name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing required argument: {name}"))
}
fn nullable_string(a: &Map<String, Value>, name: &str) -> Result<Option<String>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.clone())),
        _ => Err(format!("{name} must be a string or null")),
    }
}
fn string_or(a: &Map<String, Value>, name: &str, default: &str) -> Result<String, String> {
    Ok(nullable_string(a, name)?.unwrap_or_else(|| default.into()))
}
fn required_integer(a: &Map<String, Value>, name: &str) -> Result<i64, String> {
    get(a, name)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("Missing required argument: {name}"))
}
fn nullable_integer(a: &Map<String, Value>, name: &str) -> Result<Option<i64>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer or null")),
    }
}
fn integer_or(a: &Map<String, Value>, name: &str, default: i64) -> Result<i64, String> {
    Ok(nullable_integer(a, name)?.unwrap_or(default))
}
fn nullable_boolean(a: &Map<String, Value>, name: &str) -> Result<Option<bool>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a boolean or null")),
    }
}
fn boolean_or(a: &Map<String, Value>, name: &str, default: bool) -> Result<bool, String> {
    Ok(nullable_boolean(a, name)?.unwrap_or(default))
}
fn nullable_string_array(
    a: &Map<String, Value>,
    name: &str,
) -> Result<Option<Vec<String>>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let parsed = container(v, name)?;
            parsed
                .as_array()
                .map(|x| {
                    x.iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| format!("{name} must contain only strings"))
                        })
                        .collect()
                })
                .transpose()?
                .ok_or_else(|| format!("{name} must be a list or JSON-encoded list"))
                .map(Some)
        }
    }
}
fn nullable_integer_array(a: &Map<String, Value>, name: &str) -> Result<Option<Vec<i64>>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let parsed = container(v, name)?;
            parsed
                .as_array()
                .map(|x| {
                    x.iter()
                        .map(|v| {
                            v.as_i64()
                                .ok_or_else(|| format!("{name} must contain only integers"))
                        })
                        .collect()
                })
                .transpose()?
                .ok_or_else(|| format!("{name} must be a list or JSON-encoded list"))
                .map(Some)
        }
    }
}
fn nullable_object(
    a: &Map<String, Value>,
    name: &str,
) -> Result<Option<Map<String, Value>>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => container(v, name)?
            .as_object()
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("{name} must be an object or JSON-encoded object")),
    }
}
fn nullable_values(
    a: &Map<String, Value>,
    name: &str,
    any: bool,
) -> Result<Option<Vec<Vec<Value>>>, String> {
    match get(a, name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let parsed = container(v, name)?;
            if !valid_values(&parsed, any) {
                return Err(format!("{name} must be a 2D list"));
            }
            Ok(Some(
                parsed
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row.as_array().unwrap().clone())
                    .collect(),
            ))
        }
    }
}
fn normalized_enum(
    a: &Map<String, Value>,
    name: &str,
    allowed: &[&str],
) -> Result<Option<String>, String> {
    let Some(value) = nullable_string(a, name)? else {
        return Ok(None);
    };
    let upper = value.to_ascii_uppercase();
    if allowed.contains(&upper.as_str()) {
        Ok(Some(upper))
    } else {
        Err(format!("{name} must be one of {:?}.", allowed))
    }
}

fn array<'a>(v: &'a Value, name: &str) -> &'a [Value] {
    v.get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn display_field(v: &Value, name: &str, default: &str) -> String {
    match v.get(name) {
        Some(Value::String(value)) => value.clone(),
        Some(value) => python_repr(value),
        None => default.into(),
    }
}
fn int_field(v: &Value, name: &str) -> i64 {
    v.get(name).and_then(Value::as_i64).unwrap_or(0)
}
fn python_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("'{}': {}", k.replace('\'', "\\'"), python_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn scalar_string(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn extended_value(v: &Value, formulas: bool) -> Value {
    match v {
        Value::Bool(b) => json!({"boolValue":b}),
        Value::Number(n) => json!({"numberValue":n}),
        Value::String(s) if formulas && s.starts_with('=') => json!({"formulaValue":s}),
        Value::String(s) => json!({"stringValue":s}),
        _ => json!({"stringValue":python_repr(v)}),
    }
}

fn split_sheet_range(value: &str) -> (Option<String>, String) {
    if !value.contains('!') {
        return (None, value.into());
    }
    if value.starts_with('\'')
        && let Some(i) = value.find("'!")
    {
        return (Some(value[1..i].replace("''", "'")), value[i + 2..].into());
    }
    let (sheet, range) = value.split_once('!').unwrap();
    (Some(sheet.trim().trim_matches('\'').into()), range.into())
}
fn column_index(value: &str) -> Result<i64, String> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(format!("Invalid column letter: '{value}'."));
    }
    let mut out = 0i64;
    for c in value.to_ascii_uppercase().bytes() {
        out = out
            .checked_mul(26)
            .and_then(|v| v.checked_add((c - b'A' + 1) as i64))
            .ok_or("Column letter is too large")?;
    }
    Ok(out - 1)
}
fn index_column(mut index: i64) -> Result<String, String> {
    if index < 0 {
        return Err(format!("Column index must be non-negative, got {index}."));
    }
    index += 1;
    let mut out = Vec::new();
    while index > 0 {
        let r = (index - 1) % 26;
        out.push((b'A' + r as u8) as char);
        index = (index - 1) / 26;
    }
    Ok(out.iter().rev().collect())
}
fn parse_a1_part(value: &str) -> Result<(Option<i64>, Option<i64>), String> {
    let clean = value.replace('$', "");
    let split = clean
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(clean.len());
    let (col, row) = clean.split_at(split);
    if !col.chars().all(|c| c.is_ascii_alphabetic())
        || !row.chars().all(|c| c.is_ascii_digit())
        || col.is_empty() && row.is_empty()
    {
        return Err(format!("Invalid A1 range part: '{value}'."));
    }
    let col = if col.is_empty() {
        None
    } else {
        Some(column_index(col)?)
    };
    let row = if row.is_empty() {
        None
    } else {
        let n: i64 = row
            .parse()
            .map_err(|_| format!("Invalid A1 range part: '{value}'."))?;
        if n < 1 {
            return Err(format!("Invalid A1 range part: '{value}'."));
        }
        Some(n - 1)
    };
    Ok((col, row))
}
fn quote_sheet_always(title: &str) -> String {
    format!("'{}'", title.replace('\'', "''"))
}
fn quote_sheet(title: &str) -> String {
    if !title.is_empty() && title.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        title.into()
    } else {
        format!("'{}'", title.replace('\'', "''"))
    }
}
fn format_a1_part(col: Option<i64>, row: Option<i64>) -> Result<String, String> {
    let mut out = String::new();
    if let Some(c) = col {
        out.push_str(&index_column(c)?);
    }
    if let Some(r) = row {
        out.push_str(&(r + 1).to_string());
    }
    if out.is_empty() {
        Err("A1 range part must include a column and/or row.".into())
    } else {
        Ok(out)
    }
}
fn clamp_read_range(value: &str) -> Result<(String, String), String> {
    if value.starts_with('\'') && value.ends_with('\'') && !value.contains('!') {
        let changed = format!("{}!1:{}", value, MAX_READ_ROWS);
        return Ok((changed.clone(), clamp_note(value, &changed)));
    }
    let (sheet, a1) = split_sheet_range(value);
    if a1.is_empty() {
        return Ok((value.into(), String::new()));
    }
    let has_colon = a1.contains(':');
    let (start, end) = a1.split_once(':').unwrap_or((&a1, &a1));
    let Ok((sc, sr)) = parse_a1_part(start) else {
        return Ok((value.into(), String::new()));
    };
    let Ok((ec, er)) = parse_a1_part(end) else {
        return Ok((value.into(), String::new()));
    };
    if !has_colon && sr.is_none() {
        return Ok((value.into(), String::new()));
    }
    let effective = sr.unwrap_or(0);
    let max_end = effective + MAX_READ_ROWS - 1;
    let nsr = sr.or(Some(effective));
    let ner = Some(er.unwrap_or(max_end).min(max_end));
    if sr == nsr && er == ner {
        return Ok((value.into(), String::new()));
    }
    let left = format_a1_part(sc, nsr)?;
    let right = format_a1_part(ec, ner)?;
    let reference = if left == right {
        left
    } else {
        format!("{left}:{right}")
    };
    let changed = sheet
        .map(|s| format!("{}!{}", quote_sheet(&s), reference))
        .unwrap_or(reference);
    Ok((changed.clone(), clamp_note(value, &changed)))
}
fn clamp_note(original: &str, changed: &str) -> String {
    format!(
        "\n\nNote: Requested range '{}' was clamped to '{}' (max {} rows per read). Request a later row window to continue paging.",
        original, changed, MAX_READ_ROWS
    )
}
fn parse_grid_range(value: &str, sheets: &[Value]) -> Result<Value, String> {
    if sheets.is_empty() {
        return Err("Spreadsheet has no sheets.".into());
    }
    let (sheet, a1) = split_sheet_range(value);
    if a1.is_empty() {
        return Err("A1-style range must not be empty (e.g., 'A1', 'A1:B10').".into());
    }
    let selected = select_sheet(sheets, sheet.as_deref())?;
    let sid = sheet_props(selected)
        .get("sheetId")
        .cloned()
        .ok_or("API response missing sheetId")?;
    let (start, end) = a1.split_once(':').unwrap_or((&a1, &a1));
    let (sc, sr) = parse_a1_part(start)?;
    let (ec, er) = parse_a1_part(end)?;
    let mut out = Map::new();
    out.insert("sheetId".into(), sid);
    if let Some(v) = sr {
        out.insert("startRowIndex".into(), json!(v));
    }
    if let Some(v) = sc {
        out.insert("startColumnIndex".into(), json!(v));
    }
    if let Some(v) = er {
        out.insert("endRowIndex".into(), json!(v + 1));
    }
    if let Some(v) = ec {
        out.insert("endColumnIndex".into(), json!(v + 1));
    }
    Ok(Value::Object(out))
}
fn select_sheet<'a>(sheets: &'a [Value], name: Option<&str>) -> Result<&'a Value, String> {
    if sheets.is_empty() {
        return Err("Spreadsheet has no sheets.".into());
    }
    if let Some(name) = name {
        sheets
            .iter()
            .find(|s| sheet_title(s) == name)
            .ok_or_else(|| {
                format!(
                    "Sheet '{}' not found. Available sheets: {}.",
                    name,
                    sheets
                        .iter()
                        .map(sheet_title)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    } else {
        Ok(&sheets[0])
    }
}
fn sheet_props(v: &Value) -> &Map<String, Value> {
    match v.get("properties").and_then(Value::as_object) {
        Some(properties) => properties,
        None => empty_map(),
    }
}
fn empty_map() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}
fn sheet_title(v: &Value) -> &str {
    v.pointer("/properties/title")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
}
fn sheet_title_map(sheets: &[Value]) -> Map<String, Value> {
    let mut out = Map::new();
    for sheet in sheets {
        if let Some(id) = sheet.pointer("/properties/sheetId").and_then(Value::as_i64) {
            out.insert(id.to_string(), json!(sheet_title(sheet)));
        }
    }
    out
}
fn tight_range(resolved: &str, values: &[Value]) -> Option<String> {
    let rows = values.len() as i64;
    if rows == 0 {
        return None;
    }
    let cols = values
        .iter()
        .filter_map(Value::as_array)
        .map(Vec::len)
        .max()? as i64;
    if cols == 0 {
        return None;
    }
    let (sheet, a1) = split_sheet_range(resolved);
    let start = a1.split(':').next()?;
    let (col, row) = parse_a1_part(start).ok()?;
    let c = col.unwrap_or(0);
    let r = row.unwrap_or(0);
    let range = format!(
        "{}{}:{}{}",
        index_column(c).ok()?,
        r + 1,
        index_column(c + cols - 1).ok()?,
        r + rows
    );
    Some(
        sheet
            .map(|s| format!("{}!{}", quote_sheet(&s), range))
            .unwrap_or(range),
    )
}

fn parse_color(raw: &str) -> Result<Value, String> {
    let value = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if value.len() != 6 {
        return Err(format!(
            "Color '{}' must be in format #RRGGBB or RRGGBB.",
            raw
        ));
    }
    let parse = |part: &str| {
        u8::from_str_radix(part, 16)
            .map(|v| v as f64 / 255.0)
            .map_err(|_| format!("Color '{}' is not valid hex.", raw))
    };
    Ok(json!({"red":parse(&value[0..2])?,"green":parse(&value[2..4])?,"blue":parse(&value[4..6])?}))
}
fn color_hex(value: Option<&Value>) -> Option<String> {
    let c = value?.as_object()?;
    let component = |name: &str| {
        ((c.get(name).and_then(Value::as_f64).unwrap_or(0.0) * 255.0)
            .round()
            .clamp(0.0, 255.0)) as u8
    };
    Some(format!(
        "#{:02X}{:02X}{:02X}",
        component("red"),
        component("green"),
        component("blue")
    ))
}
fn condition_values(a: &Map<String, Value>) -> Result<Option<Vec<Value>>, String> {
    match a.get("condition_values") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let p = container(v, "condition_values")?;
            p.as_array()
                .cloned()
                .map(Some)
                .ok_or("condition_values must be a list or a JSON-encoded list".into())
        }
    }
}
fn gradient_points(a: &Map<String, Value>) -> Result<Option<Vec<Value>>, String> {
    let Some(v) = a.get("gradient_points") else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    let p = container(v, "gradient_points")?;
    let points = p
        .as_array()
        .ok_or("gradient_points must be a list of point objects.")?;
    if !(2..=3).contains(&points.len()) {
        return Err("Provide 2 or 3 gradient points (min/max or min/mid/max).".into());
    }
    let allowed = ["MIN", "MAX", "NUMBER", "PERCENT", "PERCENTILE"];
    let mut out = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let obj = p
            .as_object()
            .ok_or_else(|| format!("gradient_points[{i}] must be an object with type/color."))?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_ascii_uppercase)
            .ok_or_else(|| format!("gradient_points[{i}].type must be one of {:?}.", allowed))?;
        if !allowed.contains(&kind.as_str()) {
            return Err(format!(
                "gradient_points[{i}].type must be one of {:?}.",
                allowed
            ));
        }
        let color = obj
            .get("color")
            .ok_or_else(|| format!("gradient_points[{i}].color is required."))?;
        let color = if let Some(s) = color.as_str() {
            parse_color(s)?
        } else if color.is_object() {
            color.clone()
        } else {
            return Err(format!("gradient_points[{i}].color is required."));
        };
        let mut point = json!({"type":kind,"color":color});
        if let Some(v) = obj.get("value").filter(|v| !v.is_null()) {
            point["value"] = json!(scalar_string(v));
        }
        out.push(point);
    }
    Ok(Some(out))
}
fn build_boolean(
    ranges: Vec<Value>,
    kind: &str,
    values: Option<&[Value]>,
    bg: Option<&str>,
    fg: Option<&str>,
) -> Result<(Value, String), String> {
    if bg.is_none() && fg.is_none() {
        return Err(
            "Provide at least one of background_color or text_color for the rule format.".into(),
        );
    }
    let kind = kind.to_ascii_uppercase();
    if !CONDITION_TYPES.contains(&kind.as_str()) {
        return Err(condition_type_error());
    }
    let mut condition = json!({"type":kind});
    if let Some(v) = values.filter(|v| !v.is_empty()) {
        condition["values"] = Value::Array(
            v.iter()
                .map(|x| json!({"userEnteredValue":scalar_string(x)}))
                .collect(),
        );
    }
    let mut format = Map::new();
    if let Some(v) = bg {
        format.insert("backgroundColor".into(), parse_color(v)?);
    }
    if let Some(v) = fg {
        format.insert(
            "textFormat".into(),
            json!({"foregroundColor":parse_color(v)?}),
        );
    }
    Ok((
        json!({"ranges":ranges,"booleanRule":{"condition":condition,"format":format}}),
        kind,
    ))
}
fn build_gradient(ranges: Vec<Value>, points: Vec<Value>) -> Value {
    let mut gradient = Map::new();
    gradient.insert("minpoint".into(), points[0].clone());
    if points.len() == 3 {
        gradient.insert("midpoint".into(), points[1].clone());
    }
    gradient.insert("maxpoint".into(), points[points.len() - 1].clone());
    json!({"ranges":ranges,"gradientRule":gradient})
}
fn condition_type_error() -> String {
    format!("condition_type must be one of {:?}.", CONDITION_TYPES)
}
fn values_desc(values: Option<&[Value]>) -> String {
    values
        .filter(|v| !v.is_empty())
        .map(|v| format!(" with values {}", python_repr(&Value::Array(v.to_vec()))))
        .unwrap_or_default()
}
fn format_rules(title: &str, rules: &[Value], titles: &Map<String, Value>, indent: &str) -> String {
    if rules.is_empty() {
        return format!("{indent}Conditional formats for \"{title}\": none.");
    }
    let mut lines = vec![format!(
        "{indent}Conditional formats for \"{title}\" ({}):",
        rules.len()
    )];
    for (i, rule) in rules.iter().enumerate() {
        let ranges = array(rule, "ranges")
            .iter()
            .map(|r| grid_a1(r, titles))
            .collect::<Vec<_>>();
        let ranges = if ranges.is_empty() {
            "(no range)".into()
        } else {
            ranges.join(", ")
        };
        if let Some(b) = rule.get("booleanRule") {
            let kind = b
                .pointer("/condition/type")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN");
            let vals = array(b.get("condition").unwrap_or(&Value::Null), "values")
                .iter()
                .filter_map(|v| v.get("userEnteredValue"))
                .cloned()
                .collect::<Vec<_>>();
            let mut fmt = Vec::new();
            if let Some(c) = color_hex(b.pointer("/format/backgroundColor")) {
                fmt.push(format!("bg {c}"));
            }
            if let Some(c) = color_hex(b.pointer("/format/textFormat/foregroundColor")) {
                fmt.push(format!("text {c}"));
            }
            lines.push(format!(
                "{indent}  [{i}] {kind}{} -> {} on {ranges}",
                values_desc(Some(&vals)),
                if fmt.is_empty() {
                    "no format".into()
                } else {
                    fmt.join(", ")
                }
            ));
        } else if let Some(g) = rule.get("gradientRule") {
            let mut points = Vec::new();
            for key in ["minpoint", "midpoint", "maxpoint"] {
                if let Some(p) = g.get(key) {
                    let mut d = p
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or(key)
                        .to_string();
                    if let Some(v) = p.get("value").and_then(Value::as_str) {
                        d.push_str(&format!(":{v}"));
                    }
                    if let Some(c) = color_hex(p.get("color")) {
                        d.push_str(&format!(" {c}"));
                    }
                    points.push(d);
                }
            }
            lines.push(format!(
                "{indent}  [{i}] gradient -> {} on {ranges}",
                if points.is_empty() {
                    "gradient".into()
                } else {
                    points.join(" | ")
                }
            ));
        } else {
            lines.push(format!("{indent}  [{i}] (unknown rule) on {ranges}"));
        }
    }
    lines.join("\n")
}
fn grid_a1(range: &Value, titles: &Map<String, Value>) -> String {
    let sid = range.get("sheetId").and_then(Value::as_i64);
    let title = sid
        .and_then(|id| titles.get(&id.to_string()))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "Sheet {}",
                sid.map(|v| v.to_string()).unwrap_or_else(|| "None".into())
            )
        });
    let sr = range.get("startRowIndex").and_then(Value::as_i64);
    let er = range
        .get("endRowIndex")
        .and_then(Value::as_i64)
        .map(|v| v - 1);
    let sc = range.get("startColumnIndex").and_then(Value::as_i64);
    let ec = range
        .get("endColumnIndex")
        .and_then(Value::as_i64)
        .map(|v| v - 1);
    if sr.is_none() && er.is_none() && sc.is_none() && ec.is_none() {
        return title;
    }
    let start = format_a1_part(sc, sr).unwrap_or_default();
    let end = format_a1_part(ec, er).unwrap_or_default();
    let reference = if start == end {
        start
    } else if start.is_empty() {
        end
    } else if end.is_empty() {
        start
    } else {
        format!("{start}:{end}")
    };
    format!("{title}!{reference}")
}

fn dimension_property(
    sid: i64,
    dimension: &str,
    start: i64,
    end: i64,
    field: &str,
    value: Value,
) -> Value {
    let mut properties = Map::new();
    properties.insert(field.to_string(), value);
    json!({"updateDimensionProperties":{"range":{"sheetId":sid,"dimension":dimension,"startIndex":start,"endIndex":end},"properties":properties,"fields":field}})
}
fn delete_dimension(sid: i64, dimension: &str, start: i64, end: i64) -> Value {
    json!({"deleteDimension":{"range":{"sheetId":sid,"dimension":dimension,"startIndex":start,"endIndex":end}}})
}
fn positive_number(value: &Value, label: &str) -> Result<i64, String> {
    let number = value
        .as_f64()
        .filter(|v| *v > 0.0)
        .ok_or_else(|| format!("{label} must be a positive number."))?;
    Ok(number as i64)
}
fn add_auto_resize(
    a: &Map<String, Value>,
    name: &str,
    sid: i64,
    dimension: &str,
    requests: &mut Vec<Value>,
    summary: &mut Vec<String>,
) -> Result<(), String> {
    if dimension == "COLUMNS" {
        if let Some(items) = nullable_string_array(a, name)? {
            for item in &items {
                let i = column_index(item)?;
                requests.push(json!({"autoResizeDimensions":{"dimensions":{"sheetId":sid,"dimension":dimension,"startIndex":i,"endIndex":i+1}}}));
            }
            summary.push(format!("auto-resized columns: {}", items.join(", ")));
        }
    } else if let Some(items) = nullable_integer_array(a, name)? {
        for n in &items {
            if *n < 1 {
                return Err(format!("Row number must be >= 1, got {n}."));
            }
            requests.push(json!({"autoResizeDimensions":{"dimensions":{"sheetId":sid,"dimension":dimension,"startIndex":n-1,"endIndex":n}}}));
        }
        summary.push(format!(
            "auto-resized rows: {}",
            items
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}
fn add_insert(
    a: &Map<String, Value>,
    sid: i64,
    dimension: &str,
    count_name: &str,
    at_name: &str,
    requests: &mut Vec<Value>,
    summary: &mut Vec<String>,
) -> Result<(), String> {
    let Some(count) = nullable_integer(a, count_name)? else {
        return Ok(());
    };
    if count < 1 {
        return Err(format!("{count_name} must be a positive integer."));
    }
    let at = if dimension == "ROWS" {
        nullable_integer(a, at_name)?
    } else {
        nullable_string(a, at_name)?
            .map(|v| column_index(&v))
            .transpose()?
    };
    if let Some(at) = at {
        if dimension == "ROWS" && at < 1 {
            return Err("insert_rows_at must be a positive integer (1-based).".into());
        }
        let start = if dimension == "ROWS" { at - 1 } else { at };
        requests.push(json!({"insertDimension":{"range":{"sheetId":sid,"dimension":dimension,"startIndex":start,"endIndex":start+count},"inheritFromBefore":start>0}}));
        summary.push(format!(
            "inserted {count} {}(s) at {} {}",
            if dimension == "ROWS" { "row" } else { "column" },
            if dimension == "ROWS" { "row" } else { "column" },
            if dimension == "ROWS" {
                at.to_string()
            } else {
                index_column(at)?
            }
        ));
    } else {
        requests
            .push(json!({"appendDimension":{"sheetId":sid,"dimension":dimension,"length":count}}));
        summary.push(format!(
            "appended {count} {}(s)",
            if dimension == "ROWS" { "row" } else { "column" }
        ));
    }
    Ok(())
}
fn parse_row_range(value: &str) -> Result<(i64, i64), String> {
    let (a, b) = value.split_once(':').ok_or_else(|| {
        format!("delete_row_range must be a 'start:end' string (e.g. '5:10'), got: '{value}'.")
    })?;
    let start:i64=a.parse().map_err(|_|format!("Invalid delete_row_range format: '{value}'. Expected 'start:end' with integer row numbers."))?;
    let end:i64=b.parse().map_err(|_|format!("Invalid delete_row_range format: '{value}'. Expected 'start:end' with integer row numbers."))?;
    if start < 1 || end < start {
        return Err(format!(
            "Invalid row range: start={start}, end={end}. Rows are 1-based and end must be >= start."
        ));
    }
    Ok((start, end))
}

async fn fetch_grid_metadata(
    session: &GoogleSession,
    id: &str,
    range: &str,
    links: bool,
    notes: bool,
) -> Result<Value, String> {
    let mut vf = Vec::new();
    if links {
        vf.extend(["hyperlink", "textFormatRuns(format(link(uri)))"]);
    }
    if notes {
        vf.push("note");
    }
    let fields = format!(
        "sheets(properties(title),data(startRow,startColumn,rowData(values({}))))",
        vf.join(",")
    );
    spreadsheet_get(
        session,
        "read_sheet_values",
        id,
        &fields,
        &[("ranges", range), ("includeGridData", "true")],
    )
    .await
}
async fn fetch_error_grid(session: &GoogleSession, id: &str, range: &str) -> Result<Value, String> {
    spreadsheet_get(session,"read_sheet_values",id,"sheets(properties(title),data(startRow,startColumn,rowData(values(effectiveValue(errorValue)))))",&[("ranges",range),("includeGridData","true")]).await
}
fn walk_cells<F: FnMut(&str, i64, i64, &Value)>(grid: &Value, mut f: F) {
    for sheet in array(grid, "sheets") {
        let title = sheet_title(sheet);
        for data in array(sheet, "data") {
            let sr = data.get("startRow").and_then(Value::as_i64).unwrap_or(0);
            let sc = data.get("startColumn").and_then(Value::as_i64).unwrap_or(0);
            for (ri, row) in array(data, "rowData").iter().enumerate() {
                for (ci, cell) in array(row, "values").iter().enumerate() {
                    f(title, sr + ri as i64, sc + ci as i64, cell);
                }
            }
        }
    }
}
fn cell_ref(title: &str, row: i64, col: i64) -> String {
    format!(
        "{}!{}{}",
        quote_sheet(title),
        index_column(col).unwrap_or_else(|_| "?".into()),
        row + 1
    )
}
fn format_hyperlinks(grid: &Value, range: &str) -> String {
    let mut values = Vec::new();
    walk_cells(grid, |title, row, col, cell| {
        if let Some(link) = cell.get("hyperlink").and_then(Value::as_str) {
            values.push((cell_ref(title, row, col), link.to_string()));
        }
        for run in array(cell, "textFormatRuns") {
            if let Some(link) = run.pointer("/format/link/uri").and_then(Value::as_str) {
                values.push((cell_ref(title, row, col), link.to_string()));
            }
        }
    });
    format_pairs("Hyperlinks", range, &values, 25)
}
fn format_notes(grid: &Value, range: &str) -> String {
    let mut values = Vec::new();
    walk_cells(grid, |title, row, col, cell| {
        if let Some(note) = cell.get("note").and_then(Value::as_str) {
            values.push((cell_ref(title, row, col), note.to_string()));
        }
    });
    format_pairs("Cell notes", range, &values, 25)
}
fn format_errors(grid: &Value, range: &str) -> String {
    let mut values = Vec::new();
    walk_cells(grid, |title, row, col, cell| {
        if let Some(error) = cell
            .pointer("/effectiveValue/errorValue")
            .or_else(|| cell.get("errorValue"))
        {
            let kind = error.get("type").and_then(Value::as_str).unwrap_or("");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            values.push((
                cell_ref(title, row, col),
                if !kind.is_empty() && !message.is_empty() {
                    format!("{kind} — {message}")
                } else {
                    format!("{kind}{message}")
                },
            ));
        }
    });
    format_pairs("Detailed cell errors", range, &values, 25)
}
fn format_pairs(title: &str, range: &str, values: &[(String, String)], max: usize) -> String {
    if values.is_empty() {
        return String::new();
    }
    let lines = values
        .iter()
        .take(max)
        .map(|(a, b)| format!("- {a}: {b}"))
        .collect::<Vec<_>>();
    let suffix = if values.len() > max {
        format!("\n... and {} more", values.len() - max)
    } else {
        String::new()
    };
    format!(
        "\n\n{title} in range '{range}':\n{}{suffix}",
        lines.join("\n")
    )
}
fn format_formulas(resolved: &str, rows: &[Value]) -> String {
    let (sheet, a1) = split_sheet_range(resolved);
    let (base_col, base_row) =
        parse_a1_part(a1.split(':').next().unwrap_or("A1")).unwrap_or((Some(0), Some(0)));
    let mut values = Vec::new();
    for (ri, row) in rows.iter().filter_map(Value::as_array).enumerate() {
        for (ci, value) in row.iter().enumerate() {
            if let Some(formula) = value.as_str().filter(|v| v.starts_with('=')) {
                let cell = format!(
                    "{}{}",
                    index_column(base_col.unwrap_or(0) + ci as i64).unwrap_or_default(),
                    base_row.unwrap_or(0) + ri as i64 + 1
                );
                values.push((
                    sheet
                        .as_deref()
                        .map(|s| format!("{}!{}", quote_sheet(s), cell))
                        .unwrap_or(cell),
                    formula.to_string(),
                ));
            }
        }
    }
    format_pairs("Formula cells", resolved, &values, 50)
}
fn values_contain_errors(rows: &[Value]) -> bool {
    rows.iter().filter_map(Value::as_array).flatten().any(|v| {
        v.as_str().is_some_and(|s| {
            s == "#N/A" || s.starts_with('#') && (s.ends_with('!') || s.ends_with('?'))
        })
    })
}
