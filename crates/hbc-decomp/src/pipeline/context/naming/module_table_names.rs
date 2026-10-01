// Name a Metro module that every other pass left unnamed, from the function
// table. The factory's own name, or the name of the function its `default`
// export points at. Both are strings the bytecode stored. `<anonymous>`,
// `argN`, and other placeholders are not names.

use crate::analysis::metro::{is_generic_module_specifier, MetroRegistry};
use crate::file::BytecodeFile;

pub(super) fn name_unnamed_modules_from_function_table(
    file: &BytecodeFile,
    registry: &mut MetroRegistry,
) -> usize {
    let ids: Vec<u32> = registry
        .modules
        .iter()
        .filter(|(_, m)| m.name.is_none())
        .map(|(id, _)| *id)
        .collect();
    let mut named = 0;
    for id in ids {
        let Some(module) = registry.modules.get(&id) else {
            continue;
        };
        let chosen = function_table_name(file, module.function_id).or_else(|| {
            module
                .exports
                .get("default")
                .copied()
                .and_then(|fid| function_table_name(file, fid))
        });
        let Some(name) = chosen else { continue };
        if let Some(module) = registry.modules.get_mut(&id) {
            module.name = Some(name);
            named += 1;
        }
    }
    named
}

fn function_table_name(file: &BytecodeFile, func_id: u32) -> Option<String> {
    let raw = file
        .function_headers
        .get(func_id as usize)
        .and_then(|h| file.string_at(h.function_name()))
        .map(|e| e.value.clone())?;
    let name = raw.trim();
    if name.is_empty() || name == "anonymous" || name == "<anonymous>" || name == "?" {
        return None;
    }
    if !crate::util::is_valid_identifier(name) || is_generic_module_specifier(name) {
        return None;
    }
    if name.starts_with("arg") && name[3..].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(name.to_string())
}
