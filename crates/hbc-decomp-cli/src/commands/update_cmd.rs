use self_update::backends::github::Update;
use self_update::update::ReleaseUpdate;

const REPO_OWNER: &str = "SymbioticSec";
const REPO_NAME: &str = "hermes-decomp";
const BIN_NAME: &str = "hermes-decomp";
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

fn target_triple() -> &'static str {
    if cfg!(target_os = "linux") {
        if cfg!(target_arch = "x86_64") {
            "linux-x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "linux-arm64"
        } else {
            "linux"
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else if cfg!(target_arch = "x86_64") {
            "macos-x86_64"
        } else {
            "macos"
        }
    } else if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            "windows-x86_64"
        } else {
            "windows"
        }
    } else {
        "unknown"
    }
}

fn build_updater(target_suffix: &str) -> Result<Box<dyn ReleaseUpdate>, Box<dyn std::error::Error>> {
    let asset_name = format!("hermes-decomp-{}.{}", target_suffix, archive_ext());
    let updater = Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(PKG_VERSION)
        .target(&asset_name)
        .show_download_progress(true)
        .show_output(false)
        .no_confirm(true)
        .build()?;
    Ok(updater)
}

fn archive_ext() -> &'static str {
    if cfg!(target_os = "windows") { "zip" } else { "tar.gz" }
}

pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub notes: Option<String>,
    pub update_available: bool,
}

pub fn check() -> Result<UpdateInfo, Box<dyn std::error::Error>> {
    let target = target_triple();
    let updater = build_updater(target)?;
    let latest = updater.get_latest_release()?;
    let latest_raw = latest.name.trim_start_matches('v').to_string();
    let latest_tag = normalize_semver(&latest_raw);
    let current_tag = normalize_semver(PKG_VERSION);
    let update_available = is_newer(&latest_raw, PKG_VERSION);
    Ok(UpdateInfo {
        current: current_tag,
        latest: latest_tag,
        notes: latest.body,
        update_available,
    })
}

pub fn install(version: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let target = target_triple();
    let updater = if let Some(v) = version {
        let v = v.trim_start_matches('v');
        let asset_name = format!("hermes-decomp-{}.{}", target, archive_ext());
        Update::configure()
            .repo_owner(REPO_OWNER)
            .repo_name(REPO_NAME)
            .bin_name(BIN_NAME)
            .current_version(PKG_VERSION)
            .target_version_tag(v)
            .target(&asset_name)
            .show_download_progress(true)
            .show_output(false)
            .no_confirm(true)
            .build()?
    } else {
        build_updater(target)?
    };
    let status = updater.update()?;
    log::info!("Update status: `{}`", status);
    println!("Downloaded and installed. Restart hermes-decomp to use the new version.");
    println!("(self_update reported: {:?})", status);
    Ok(())
}

fn is_newer(latest: &str, current: &str) -> bool {
    parse_semver(latest) > parse_semver(current)
}

fn parse_semver(s: &str) -> Vec<u64> {
    s.trim_start_matches('v')
        .split(|c: char| c == '.' || c == '-' || c == '+')
        .filter_map(|p| p.parse::<u64>().ok())
        .collect()
}

fn normalize_semver(s: &str) -> String {
    parse_semver(s)
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

pub fn run(check_only: bool, install_now: bool, version: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let do_install = install_now || (!check_only && version.is_some());

    if do_install {
        if version.is_none() {
            let info = check()?;
            if !info.update_available {
                println!("Already on the latest version ({}).", info.current);
                return Ok(());
            }
            println!("Updating from v{} to v{}...", info.current, info.latest);
            if let Some(notes) = info.notes.as_deref() {
                println!("\nChangelog:\n{}", notes);
            }
        }
        install(version.as_deref())?;
        return Ok(());
    }

    let info = check()?;
    if info.update_available {
        println!("Update available: v{} -> v{}", info.current, info.latest);
        if let Some(notes) = info.notes.as_deref() {
            println!("\nChangelog:\n{}", notes);
        }
        println!("\nRun `hermes-decomp update --install` to upgrade.");
    } else {
        println!("Up to date (v{}).", info.current);
    }
    Ok(())
}

pub fn auto_check_on_startup() {
    if std::env::var_os("HERMES_DECOMP_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let enabled = std::env::var("HERMES_DECOMP_UPDATE_CHECK")
        .map(|v| !matches!(v.as_str(), "0" | "false" | "no" | "off"))
        .unwrap_or(false);
    if !enabled {
        return;
    }
    std::thread::spawn(|| {
        if let Ok(info) = check() {
            if info.update_available {
                eprintln!(
                    "hermes-decomp v{} is available (you have v{}). Run `hermes-decomp update --install` to upgrade.",
                    info.latest, info.current
                );
            }
        }
    });
}
