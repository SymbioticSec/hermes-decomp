use crate::cli_args::XrefKind;
use crate::helpers::function_name;
use hbc_decomp::analysis::XrefResult;
use hbc_decomp::{BytecodeFile, BytecodeFormat};
use serde_json::{json, Value};
use std::error::Error;

pub fn run_xref(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    query: &str,
    kind: XrefKind,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let results = match kind {
        XrefKind::String => hbc_decomp::analysis::find_string_xrefs(file, format, query),
        XrefKind::Function => {
            let fid = query.parse::<u32>().map_err(|_| "Invalid function ID")?;
            hbc_decomp::analysis::find_function_refs(file, format, fid)
        }
    };

    if json {
        let doc = xref_json(file, query, kind, &results);
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }

    println!("Found {} cross-references for '{}':", results.len(), query);
    for xref in results {
        let name = file
            .string_at(file.function_headers[xref.function_id as usize].function_name())
            .map(|e| e.value.as_str())
            .unwrap_or("<anonymous>");

        println!(
            "  Function {} ({}) at offset {:04x}: {}",
            xref.function_id, name, xref.offset, xref.opcode
        );
    }
    Ok(())
}

// The same rows the text listing prints, as a JSON document.
pub fn xref_json(
    file: &BytecodeFile,
    query: &str,
    kind: XrefKind,
    results: &[XrefResult],
) -> Value {
    let kind = match kind {
        XrefKind::String => "string",
        XrefKind::Function => "function",
    };
    let rows: Vec<Value> = results
        .iter()
        .map(|x| {
            json!({
                "function_id": x.function_id,
                "name": function_name(file, x.function_id),
                "offset": x.offset,
                "opcode": x.opcode,
            })
        })
        .collect();
    json!({
        "query": query,
        "kind": kind,
        "count": results.len(),
        "results": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_file() -> BytecodeFile {
        let bytes = hbc_decomp::create_minimal(&hbc_decomp::CreateOptions {
            version: 96,
            strings: vec!["global".into()],
            ..Default::default()
        })
        .unwrap();
        BytecodeFile::parse_auto(&bytes).unwrap()
    }

    #[test]
    fn xref_json_lists_every_hit() {
        let file = minimal_file();
        let results = vec![
            XrefResult {
                function_id: 0,
                offset: 4,
                opcode: "GetById".into(),
            },
            XrefResult {
                function_id: 0,
                offset: 12,
                opcode: "Call".into(),
            },
        ];
        let doc = xref_json(&file, "global", XrefKind::String, &results);
        let text = serde_json::to_string(&doc).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["query"], "global");
        assert_eq!(back["kind"], "string");
        assert_eq!(back["count"], 2);
        assert_eq!(back["results"][1]["offset"], 12);
        assert_eq!(back["results"][1]["opcode"], "Call");
        assert_eq!(back["results"][0]["function_id"], 0);
    }
}
