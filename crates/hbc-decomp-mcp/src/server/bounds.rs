// Pure helpers for the bounded, filtered `decompile_all` tool: module
// selector parsing (same grammar as the CLI flags) and output truncation.

// Parse "100-150,200" into inclusive id ranges. Malformed parts are skipped,
// a reversed range is normalized. Same grammar as the CLI `--modules` flag.
pub fn parse_id_ranges(spec: Option<&str>) -> Vec<(u32, u32)> {
    let spec = match spec {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut ranges = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((lo, hi)) = part.split_once('-') {
            if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u32>(), hi.trim().parse::<u32>()) {
                ranges.push((lo.min(hi), lo.max(hi)));
            }
        } else if let Ok(n) = part.parse::<u32>() {
            ranges.push((n, n));
        }
    }
    ranges
}

// Parse "react*,lodash*" into trimmed, non empty globs.
pub fn parse_globs(spec: Option<&str>) -> Vec<String> {
    spec.map(|s| {
        s.split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

pub struct Truncated {
    pub text: String,
    pub truncated: bool,
    pub total_chars: usize,
    pub kept_chars: usize,
}

// Cut `text` to at most `max_chars` characters at a line boundary and append a
// marker line so the client knows the output is partial. Below the limit the
// text is returned untouched.
pub fn truncate_at_line(text: String, max_chars: usize) -> Truncated {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return Truncated {
            kept_chars: total_chars,
            text,
            truncated: false,
            total_chars,
        };
    }
    // Byte offset of the char boundary after `max_chars` chars.
    let limit = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    // Back up to the last newline so a line is never split in the middle.
    let cut = text[..limit].rfind('\n').map(|i| i + 1).unwrap_or(limit);
    let mut out = text[..cut].to_string();
    let kept_chars = out.chars().count();
    out.push_str(&format!(
        "// truncated: {kept_chars} of {total_chars} chars, narrow with modules/module_name/from_module\n"
    ));
    Truncated {
        text: out,
        truncated: true,
        total_chars,
        kept_chars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse_lists_and_spans() {
        assert_eq!(parse_id_ranges(None), Vec::new());
        assert_eq!(parse_id_ranges(Some("")), Vec::new());
        assert_eq!(
            parse_id_ranges(Some("100-150, 200,,7")),
            vec![(100, 150), (200, 200), (7, 7)]
        );
        // A reversed span is normalized, a malformed part is skipped.
        assert_eq!(parse_id_ranges(Some("9-3,abc,4-x")), vec![(3, 9)]);
    }

    #[test]
    fn globs_are_trimmed_and_non_empty() {
        assert!(parse_globs(None).is_empty());
        assert_eq!(
            parse_globs(Some(" react* ,, lodash*")),
            vec!["react*".to_string(), "lodash*".to_string()]
        );
    }

    #[test]
    fn short_output_is_untouched() {
        let t = truncate_at_line("a\nb\n".to_string(), 10);
        assert!(!t.truncated);
        assert_eq!(t.text, "a\nb\n");
        assert_eq!(t.total_chars, 4);
        assert_eq!(t.kept_chars, 4);
    }

    #[test]
    fn long_output_is_cut_at_a_line_boundary() {
        let input = "line one\nline two\nline three\n".to_string();
        let t = truncate_at_line(input, 20);
        assert!(t.truncated);
        assert_eq!(t.total_chars, 29);
        assert_eq!(t.kept_chars, 18);
        assert!(t
            .text
            .starts_with("line one\nline two\n// truncated: 18 of 29 chars"));
        assert!(t
            .text
            .ends_with("narrow with modules/module_name/from_module\n"));
    }

    #[test]
    fn multibyte_text_is_cut_on_char_boundaries() {
        let input = "éé\nàà\nüü\n".to_string();
        let t = truncate_at_line(input, 5);
        assert!(t.truncated);
        assert_eq!(t.kept_chars, 3);
        assert!(t.text.starts_with("éé\n// truncated"));
    }

    #[test]
    fn single_long_line_falls_back_to_char_cut() {
        let t = truncate_at_line("abcdefgh".to_string(), 3);
        assert!(t.truncated);
        assert!(t.text.starts_with("abc// truncated: 3 of 8 chars"));
    }
}
