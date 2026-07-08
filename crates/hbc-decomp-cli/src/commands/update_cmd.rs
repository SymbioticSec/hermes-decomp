// Self-update via GitHub releases.
//
// Uses a lightweight synchronous stack (ureq, no tokio/hyper). The downloaded
// archive is verified against the release `SHA256SUMS` before the running
// binary is replaced, and version comparison uses the `semver` crate.

use std::error::Error;
use std::io::Read;

const REPO: &str = "SymbioticSec/hermes-decomp";
const BIN_NAME: &str = "hermes-decomp";
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("hermes-decomp/", env!("CARGO_PKG_VERSION"));
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024; // 256 MiB cap on any download

type Res<T> = Result<T, Box<dyn Error>>;

// The platform suffix used in release asset names, or `None` if unsupported.
fn platform_suffix() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

fn archive_ext() -> &'static str {
    if cfg!(windows) {
        "zip"
    } else {
        "tar.gz"
    }
}

// Exact release asset name for this platform, e.g.
// `hermes-decomp-v0.1.6-macos-arm64.tar.gz`. `tag` is the release tag (`v0.1.6`).
fn asset_name(tag: &str, suffix: &str) -> String {
    format!("hermes-decomp-{tag}-{suffix}.{}", archive_ext())
}

struct Release {
    tag: String,
    version: semver::Version,
    body: Option<String>,
    assets: Vec<(String, String)>, // (name, download_url)
}

impl Release {
    fn asset_url(&self, name: &str) -> Option<&str> {
        self.assets
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, u)| u.as_str())
    }
}

fn parse_version(tag: &str) -> Result<semver::Version, semver::Error> {
    semver::Version::parse(tag.trim_start_matches('v'))
}

fn http_get(url: &str) -> Res<ureq::Response> {
    ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.into())
}

fn get_release(tag: Option<&str>) -> Res<Release> {
    let url = match tag {
        Some(t) => format!("https://api.github.com/repos/{REPO}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };
    let text = http_get(&url)?.into_string()?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    parse_release(&json)
}

// Parse a GitHub release JSON object into a `Release` (pure, unit-testable).
fn parse_release(json: &serde_json::Value) -> Res<Release> {
    let tag = json["tag_name"]
        .as_str()
        .ok_or("release JSON has no tag_name")?
        .to_string();
    let version = parse_version(&tag)?;
    let body = json["body"].as_str().map(str::to_string);
    let assets = json["assets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some((
                        a["name"].as_str()?.to_string(),
                        a["browser_download_url"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        tag,
        version,
        body,
        assets,
    })
}

fn download_bytes(url: &str) -> Res<Vec<u8>> {
    let resp = ureq::get(url).set("User-Agent", USER_AGENT).call()?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// Look up the expected SHA-256 for `filename` inside a `SHA256SUMS` body.
// Each line is `<hex>  <name>` (a leading `*` on the name, from binary mode, is
// tolerated).
fn expected_sha256(sha256sums: &str, filename: &str) -> Option<String> {
    for line in sha256sums.lines() {
        let mut parts = line.split_whitespace();
        let sum = parts.next()?;
        let name = parts.next()?;
        if name.trim_start_matches('*') == filename {
            return Some(sum.to_lowercase());
        }
    }
    None
}

// Extract the `hermes-decomp` binary bytes from the downloaded archive.
#[cfg(not(windows))]
fn extract_binary(archive: &[u8]) -> Res<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        if path == BIN_NAME || path.ends_with(&format!("/{BIN_NAME}")) {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    Err(format!("`{BIN_NAME}` not found in the downloaded archive").into())
}

#[cfg(windows)]
fn extract_binary(archive: &[u8]) -> Res<Vec<u8>> {
    let reader = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader)?;
    let want = format!("{BIN_NAME}.exe");
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let name = file.name().to_string();
        if name == want || name.ends_with(&format!("/{want}")) {
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    Err(format!("`{BIN_NAME}.exe` not found in the downloaded archive").into())
}

// Stage the verified binary in a fresh file, then swap it into place.
//
// This function intentionally does NOT resolve the running executable's own
// path: that lookup is documented as unsafe to trust for security-sensitive
// decisions (it can be influenced through hard links, mount namespaces or
// `/proc` manipulation). Locating and atomically replacing the running
// executable is delegated entirely to `self_replace`. The staging file is
// opened with `create_new` (O_EXCL), which fails rather than following a
// pre-existing symlink at the path.
fn replace_running_binary(binary: &[u8]) -> Res<()> {
    use std::io::Write;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut staged_path = std::env::temp_dir();
    staged_path.push(format!("hermes-decomp-update-{}-{nonce}", std::process::id()));

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_path)?;
    file.write_all(binary)?;
    file.flush()?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let result = self_replace::self_replace(&staged_path);
    let _ = std::fs::remove_file(&staged_path);
    result.map_err(Into::into)
}

pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub notes: Option<String>,
    pub update_available: bool,
}

pub fn check() -> Res<UpdateInfo> {
    let release = get_release(None)?;
    let current = parse_version(PKG_VERSION)?;
    let update_available = release.version > current;
    Ok(UpdateInfo {
        current: current.to_string(),
        latest: release.version.to_string(),
        notes: release.body,
        update_available,
    })
}

pub fn install(version: Option<&str>) -> Res<()> {
    let suffix = platform_suffix()
        .ok_or_else(|| format!("no prebuilt binary for {}-{}", std::env::consts::OS, std::env::consts::ARCH))?;

    let tag = version.map(|v| {
        if v.starts_with('v') {
            v.to_string()
        } else {
            format!("v{v}")
        }
    });
    let release = get_release(tag.as_deref())?;

    let asset = asset_name(&release.tag, suffix);
    let asset_url = release
        .asset_url(&asset)
        .ok_or_else(|| format!("release {} has no asset `{asset}`", release.tag))?
        .to_string();

    // 1. Download the archive.
    println!("Downloading {asset}...");
    let archive = download_bytes(&asset_url)?;

    // 2. Verify its SHA-256 against the release SHA256SUMS.
    let sums_url = release
        .asset_url("SHA256SUMS")
        .ok_or("release has no SHA256SUMS asset to verify against")?;
    let sums = String::from_utf8(download_bytes(sums_url)?)?;
    let expected = expected_sha256(&sums, &asset)
        .ok_or_else(|| format!("SHA256SUMS has no entry for `{asset}`"))?;
    let actual = sha256_hex(&archive);
    if actual != expected {
        return Err(format!(
            "checksum mismatch for `{asset}`\n  expected {expected}\n  actual   {actual}"
        )
        .into());
    }
    println!("Checksum OK ({}).", &actual[..16]);

    // 3. Extract the binary and swap it in.
    let binary = extract_binary(&archive)?;
    replace_running_binary(&binary)?;

    println!(
        "Updated to {}. Restart hermes-decomp to use the new version.",
        release.tag
    );
    Ok(())
}

pub fn run(check_only: bool, install_now: bool, version: Option<String>) -> Res<()> {
    let do_install = install_now || (!check_only && version.is_some());

    if do_install {
        if version.is_none() {
            let info = check()?;
            if !info.update_available {
                println!("Already on the latest version (v{}).", info.current);
                return Ok(());
            }
            println!("Updating from v{} to v{}...", info.current, info.latest);
            if let Some(notes) = info.notes.as_deref() {
                println!("\nChangelog:\n{notes}");
            }
        }
        return install(version.as_deref());
    }

    let info = check()?;
    if info.update_available {
        println!("Update available: v{} -> v{}", info.current, info.latest);
        if let Some(notes) = info.notes.as_deref() {
            println!("\nChangelog:\n{notes}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_is_correct() {
        // The old hand-rolled Vec<u64> compare got these wrong.
        assert!(parse_version("v0.1.10").unwrap() > parse_version("v0.1.9").unwrap());
        assert!(parse_version("0.2.0").unwrap() > parse_version("0.1.99").unwrap());
        // A pre-release is older than its release.
        assert!(parse_version("1.0.0-alpha").unwrap() < parse_version("1.0.0").unwrap());
        assert_eq!(
            parse_version("v1.2.3").unwrap(),
            parse_version("1.2.3").unwrap()
        );
    }

    #[test]
    fn asset_name_is_exact() {
        assert_eq!(
            asset_name("v0.1.6", "macos-arm64"),
            if cfg!(windows) {
                "hermes-decomp-v0.1.6-macos-arm64.zip"
            } else {
                "hermes-decomp-v0.1.6-macos-arm64.tar.gz"
            }
        );
    }

    #[test]
    fn parse_sha256sums_matches_and_tolerates_star() {
        let sums = "\
abc123  hermes-decomp-v0.1.6-linux-x86_64.tar.gz
DEF456  hermes-decomp-v0.1.6-macos-arm64.tar.gz
789aaa *hermes-decomp-v0.1.6-windows-x86_64.zip
";
        assert_eq!(
            expected_sha256(sums, "hermes-decomp-v0.1.6-macos-arm64.tar.gz").as_deref(),
            Some("def456")
        );
        assert_eq!(
            expected_sha256(sums, "hermes-decomp-v0.1.6-windows-x86_64.zip").as_deref(),
            Some("789aaa")
        );
        assert_eq!(expected_sha256(sums, "not-there.tar.gz"), None);
    }

    #[test]
    fn sha256_hex_is_lowercase_hex_of_known_vector() {
        // SHA-256 of the empty input.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn parse_release_extracts_fields() {
        let json = serde_json::json!({
            "tag_name": "v0.1.6",
            "body": "notes",
            "assets": [
                {"name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS"},
                {"name": "hermes-decomp-v0.1.6-macos-arm64.tar.gz", "browser_download_url": "https://x/a"}
            ]
        });
        let rel = parse_release(&json).unwrap();
        assert_eq!(rel.tag, "v0.1.6");
        assert_eq!(rel.version, semver::Version::new(0, 1, 6));
        assert_eq!(rel.asset_url("SHA256SUMS"), Some("https://x/SHA256SUMS"));
        assert_eq!(
            rel.asset_url("hermes-decomp-v0.1.6-macos-arm64.tar.gz"),
            Some("https://x/a")
        );
        assert_eq!(rel.asset_url("missing"), None);
    }
}
