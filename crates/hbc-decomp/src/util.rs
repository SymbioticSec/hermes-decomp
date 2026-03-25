pub fn escape_js_string_bare(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            _ if ch.is_ascii() && !ch.is_control() => out.push(ch),
            _ => {
                let code = ch as u32;
                if code <= 0xFFFF {
                    out.push_str(&format!("\\u{code:04X}"));
                } else {
                    out.push_str(&format!("\\u{{{code:X}}}"));
                }
            }
        }
    }
    out
}

pub fn escape_js_string(value: &str) -> String {
    format!("\"{}\"", escape_js_string_bare(value))
}

pub fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let first = match chars.next() {
        Some(ch) => ch,
        None => return false,
    };

    if !is_identifier_start(first) {
        return false;
    }

    for ch in chars {
        if !is_identifier_part(ch) {
            return false;
        }
    }

    true
}

fn is_identifier_start(ch: char) -> bool {
    ch == '$' || ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_part(ch: char) -> bool {
    is_identifier_start(ch) || ch.is_ascii_digit()
}

// Sanitize a string to be a valid JavaScript identifier:
// - `@@symbol` -> `Symbol_symbol` (Hermes internal symbols)
// - Invalid characters -> replaced with `_`
// - Leading digits -> prefixed with `_`
pub fn sanitize_identifier(name: &str) -> String {
    if let Some(symbol_name) = name.strip_prefix("@@") {
        return format!("Symbol_{symbol_name}");
    }

    if is_valid_identifier(name) {
        return name.to_string();
    }

    let mut result = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if i == 0 {
            if is_identifier_start(ch) {
                result.push(ch);
            } else if ch.is_ascii_digit() {
                result.push('_');
                result.push(ch);
            } else {
                result.push('_');
            }
        } else if is_identifier_part(ch) {
            result.push(ch);
        } else {
            result.push('_');
        }
    }

    if result.is_empty() {
        result.push_str("_unnamed");
    }

    result
}
