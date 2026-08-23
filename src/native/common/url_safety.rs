//! Hyperlink scheme validation shared by document writers.

use percent_encoding::percent_decode_str;
use reqwest::Url;

const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];
const DANGEROUS_SCHEMES: [&str; 6] = ["javascript", "data", "vbscript", "file", "blob", "about"];
const MAX_DECODE_ROUNDS: usize = 3;

/// Validate a document hyperlink. `None` is valid and means that a link is cleared.
pub fn validate_link_url(url: Option<&str>) -> Result<(), String> {
    let Some(url) = url else {
        return Ok(());
    };
    if url.trim().is_empty() {
        return Err(String::from("link URL cannot be empty"));
    }

    let forms = candidate_forms(url)?;
    for form in &forms {
        let scheme = scheme_of(form);
        if DANGEROUS_SCHEMES.contains(&scheme) {
            return Err(format!(
                "link URL uses the disallowed scheme '{scheme}'. Allowed schemes: http, https, mailto"
            ));
        }
    }

    let primary = forms.first().map(String::as_str).unwrap_or_default();
    let scheme = scheme_of(primary);
    if !ALLOWED_SCHEMES.contains(&scheme) {
        let actual = if scheme.is_empty() {
            "no scheme"
        } else {
            scheme
        };
        return Err(format!(
            "link URL must use one of: http, https, mailto (got {actual})"
        ));
    }

    let parsed = Url::parse(primary).map_err(|_| String::from("link URL is invalid"))?;
    match scheme {
        "http" | "https" if !has_http_authority(primary) || parsed.host_str().is_none() => {
            Err(String::from("link URL must include a host"))
        }
        "mailto" if parsed.path().is_empty() => {
            Err(String::from("mailto link must include an address"))
        }
        _ => Ok(()),
    }
}

pub fn is_safe_link_url(url: Option<&str>) -> bool {
    validate_link_url(url).is_ok()
}

/// Preserve a safe URL verbatim, or drop it while allowing surrounding text to remain.
pub fn sanitize_link_url(url: Option<&str>) -> Option<String> {
    let value = url?;
    validate_link_url(Some(value))
        .is_ok()
        .then(|| value.to_owned())
}

fn candidate_forms(url: &str) -> Result<Vec<String>, String> {
    let mut forms = Vec::with_capacity(MAX_DECODE_ROUNDS + 1);
    let mut current = url.to_owned();
    for round in 0..=MAX_DECODE_ROUNDS {
        let normalized = normalize_for_scheme_check(&current);
        if !forms.contains(&normalized) {
            forms.push(normalized);
        }
        if round == MAX_DECODE_ROUNDS {
            break;
        }
        let decoded = percent_decode_str(&current)
            .decode_utf8_lossy()
            .into_owned();
        if decoded == current {
            break;
        }
        current = decoded;
    }
    Ok(forms)
}

fn normalize_for_scheme_check(url: &str) -> String {
    url.chars()
        .filter(|character| !is_strippable(*character))
        .collect()
}

fn is_strippable(character: char) -> bool {
    matches!(
        character as u32,
        0x0000..=0x0020
            | 0x007f..=0x00a0
            | 0x00ad
            | 0x200b..=0x200f
            | 0x2028
            | 0x2029
            | 0xfeff
    )
}

fn has_http_authority(url: &str) -> bool {
    let Some((_, remainder)) = url.split_once(':') else {
        return false;
    };
    let Some(authority_and_rest) = remainder.strip_prefix("//") else {
        return false;
    };
    !authority_and_rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .is_empty()
}

fn scheme_of(url: &str) -> &str {
    let Some(colon) = url.find(':') else {
        return "";
    };
    let candidate = &url[..colon];
    let mut characters = candidate.chars();
    if !characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        || !characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
    {
        return "";
    }
    if candidate.bytes().all(|byte| !byte.is_ascii_uppercase()) {
        candidate
    } else {
        // All schemes used by this module are ASCII lowercase. Returning an empty
        // marker here would reject valid uppercase schemes, so callers normalize it.
        ALLOWED_SCHEMES
            .iter()
            .chain(DANGEROUS_SCHEMES.iter())
            .copied()
            .find(|known| candidate.eq_ignore_ascii_case(known))
            .unwrap_or("")
    }
}
