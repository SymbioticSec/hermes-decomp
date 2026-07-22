// Scan the HBC string table for likely secrets / credentials.

use crate::file::BytecodeFile;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct SecretHit {
    pub string_id: u32,
    pub category: String,
    pub pattern_name: String,
    pub value: String,
    /// Value with middle redacted for safe display.
    pub redacted: String,
}

struct Pattern {
    category: &'static str,
    name: &'static str,
    re: Regex,
}

fn patterns() -> &'static [Pattern] {
    static PATS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATS.get_or_init(|| {
        let mk = |category, name, pat| Pattern {
            category,
            name,
            re: Regex::new(pat).expect("secret regex"),
        };
        vec![
            mk("aws", "aws_access_key_id", r"\bAKIA[0-9A-Z]{16}\b"),
            mk(
                "aws",
                "aws_secret_access_key",
                r#"(?i)aws.{0,20}secret.{0,20}['"][0-9a-zA-Z/+]{40}['"]"#,
            ),
            mk("google", "gcp_api_key", r"\bAIza[0-9A-Za-z\-_]{35}\b"),
            mk("stripe", "stripe_live_key", r"\bsk_live_[0-9a-zA-Z]{20,}\b"),
            mk("stripe", "stripe_test_key", r"\bsk_test_[0-9a-zA-Z]{20,}\b"),
            mk("github", "github_pat", r"\bghp_[0-9A-Za-z]{36}\b"),
            mk("github", "github_oauth", r"\bgho_[0-9A-Za-z]{36}\b"),
            mk("slack", "slack_token", r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
            mk(
                "jwt",
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            ),
            mk(
                "private_key",
                "pem_private_key",
                r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
            ),
            mk(
                "url",
                "internal_url",
                r#"https?://(?:localhost|127\.0\.0\.1|10\.\d+\.\d+\.\d+|192\.168\.\d+\.\d+)(?::\d+)?[^\s"']*"#,
            ),
            mk(
                "url",
                "api_url",
                r#"https?://[a-zA-Z0-9._-]*api[a-zA-Z0-9._-]*/[^\s"']{0,80}"#,
            ),
            mk(
                "generic",
                "bearer_token",
                r"(?i)bearer\s+[A-Za-z0-9\-._~+/]+=*",
            ),
            mk(
                "generic",
                "password_assign",
                r#"(?i)(password|passwd|pwd)\s*[:=]\s*['"][^'"]{4,}['"]"#,
            ),
            mk(
                "generic",
                "api_key_assign",
                r#"(?i)(api[_-]?key|apikey|secret[_-]?key)\s*[:=]\s*['"][^'"]{8,}['"]"#,
            ),
        ]
    })
}

fn redact(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

// Scan all strings with built-in patterns plus optional custom (name, regex).
pub fn scan_secrets(file: &BytecodeFile, extra: &[(String, Regex)]) -> Vec<SecretHit> {
    let mut hits = Vec::new();
    for (id, entry) in file.strings.iter().enumerate() {
        let val = &entry.value;
        if val.len() < 6 {
            continue;
        }
        for p in patterns() {
            if p.re.is_match(val) {
                hits.push(SecretHit {
                    string_id: id as u32,
                    category: p.category.to_string(),
                    pattern_name: p.name.to_string(),
                    value: val.clone(),
                    redacted: redact(val),
                });
            }
        }
        for (name, re) in extra {
            if re.is_match(val) {
                hits.push(SecretHit {
                    string_id: id as u32,
                    category: "custom".into(),
                    pattern_name: name.clone(),
                    value: val.clone(),
                    redacted: redact(val),
                });
            }
        }
    }
    hits
}

pub fn scan_secrets_with_custom(
    file: &BytecodeFile,
    custom: &[(String, String)],
) -> std::result::Result<Vec<SecretHit>, regex::Error> {
    let mut compiled = Vec::new();
    for (name, pat) in custom {
        compiled.push((name.clone(), Regex::new(pat)?));
    }
    Ok(scan_secrets(file, &compiled))
}

pub fn format_secrets_report(hits: &[SecretHit], redact_values: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Secret scan: {} hit(s)\n", hits.len()));
    for h in hits {
        let val = if redact_values {
            h.redacted.as_str()
        } else {
            h.value.as_str()
        };
        out.push_str(&format!(
            "[{}] {} string#{}: {}\n",
            h.category, h.pattern_name, h.string_id, val
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::StringTableEntry;
    use crate::format::{BytecodeHeader, FunctionHeaderLayout, HeaderLayout};

    fn dummy_file(strings: &[&str]) -> BytecodeFile {
        BytecodeFile {
            header: BytecodeHeader {
                magic: 0,
                version: 96,
                source_hash: [0; 20],
                file_length: 0,
                global_code_index: 0,
                function_count: 0,
                string_kind_count: 0,
                identifier_count: 0,
                string_count: strings.len() as u32,
                overflow_string_count: 0,
                string_storage_size: 0,
                big_int_count: None,
                big_int_storage_size: None,
                reg_exp_count: 0,
                reg_exp_storage_size: 0,
                literal_value_buffer_size: None,
                array_buffer_size: None,
                obj_key_buffer_size: 0,
                obj_value_buffer_size: None,
                obj_shape_table_count: None,
                num_string_switch_imms: None,
                segment_id: None,
                cjs_module_offset: None,
                cjs_module_count: 0,
                function_source_count: None,
                debug_info_offset: 0,
                options: 0,
                layout: HeaderLayout::Legacy,
                function_header_layout: FunctionHeaderLayout::Legacy16,
            },
            function_headers: vec![],
            string_kinds: vec![],
            identifier_hashes: vec![],
            strings: strings
                .iter()
                .map(|s| StringTableEntry {
                    value: (*s).into(),
                    is_utf16: false,
                    is_identifier: false,
                })
                .collect(),
            big_int_table: vec![],
            big_int_storage: vec![],
            reg_exp_table: vec![],
            reg_exp_storage: vec![],
            array_buffer: vec![],
            literal_value_buffer: vec![],
            obj_key_buffer: vec![],
            obj_value_buffer: vec![],
            obj_shape_table: vec![],
            cjs_module_table: vec![],
            function_source_table: vec![],
            instruction_offset: 0,
            instructions: vec![],
            debug_info: None,
            exception_handlers: Default::default(),
            sections: vec![],
            raw_bytes: None,
        }
    }

    #[test]
    fn detects_aws_key() {
        let f = dummy_file(&["AKIAIOSFODNN7EXAMPLE", "hello"]);
        let hits = scan_secrets(&f, &[]);
        assert!(hits.iter().any(|h| h.pattern_name == "aws_access_key_id"));
    }

    #[test]
    fn detects_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let f = dummy_file(&[jwt]);
        let hits = scan_secrets(&f, &[]);
        assert!(hits.iter().any(|h| h.pattern_name == "jwt"));
    }
}
