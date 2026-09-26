// Query the analysis cache of a bundle: for each function id given, print its
// parent chain (block scopes marked), its own slot names and, for a capture
// read `closure_{level}_{slot}`, what the resolver would see. Diagnostic only.
//
//   cargo run --release --example cache_query -- <bundle.hbc> <fid> [fid…]
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: cache_query <bundle.hbc> <fid>…");
        std::process::exit(2);
    }
    let input = std::path::PathBuf::from(&args[1]);
    let bytes = std::fs::read(&input)?;
    let file = hbc_decomp::BytecodeFile::parse_auto(&bytes)?;
    let format = hbc_decomp::BytecodeFormat::for_version(file.header.version)?;
    let options = hbc_decomp::DecompileOptionsV2::optimized();
    let cache_path = hbc_decomp::default_cache_path(&input);
    let ctx =
        hbc_decomp::PipelineContext::build_cached(&file, &format, &options, &bytes, &cache_path)?;
    let Some(cc) = ctx.closure_ctx.as_ref() else {
        println!("no closure context");
        return Ok(());
    };
    // `m<id>` prints a registry entry instead of a function.
    for arg in args[2..].iter() {
        if let Some(mid) = arg.strip_prefix('m').and_then(|m| m.parse::<u32>().ok()) {
            match ctx.registry.modules.get(&mid) {
                Some(m) => {
                    let mut exports: Vec<_> =
                        m.exports.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    exports.sort();
                    println!(
                        "== module {mid} name={:?} factory={} deps={:?} exports={:?}",
                        m.name, m.function_id, m.dependencies, exports
                    );
                }
                None => println!("== module {mid}: not in registry"),
            }
        }
    }
    for fid in args[2..].iter().filter_map(|a| a.parse::<u32>().ok()) {
        println!(
            "== function {fid} name={:?} module={:?}",
            cc.function_names.get(&fid),
            ctx.registry.function_to_module.get(&fid)
        );
        let mut cur = fid;
        let mut chain = Vec::new();
        while let Some(&p) = cc.parent_function.get(&cur) {
            let kind = if cc.is_block_scope(p) {
                format!("{p} (block scope, level {:?})", cc.block_level.get(&p))
            } else {
                p.to_string()
            };
            chain.push(kind);
            cur = p;
            if chain.len() > 12 {
                break;
            }
        }
        println!("   parents: {}", chain.join(" -> "));
        if let Some(info) = cc.function_closures.get(&fid) {
            let own: BTreeMap<u32, String> = info
                .slots
                .iter()
                .filter(|(k, _)| *k >> 24 == 0)
                .map(|(&k, v)| (k, format!("{v:?}")))
                .collect();
            println!("   own slots: {own:?}");
        }
        if let Some(baked) = cc.baked_captures.get(&fid) {
            println!("   baked captures: {baked:?}");
        }
        let merged = cc.get_closure_info_for(fid);
        let anc: Vec<String> = merged
            .slots
            .iter()
            .filter(|(k, _)| *k >> 24 != 0)
            .map(|(&k, v)| format!("L{}S{}={:?}", k >> 24, k & 0xFFFFFF, v))
            .collect();
        println!("   ancestor view: {}", anc.join(", "));
    }
    Ok(())
}
