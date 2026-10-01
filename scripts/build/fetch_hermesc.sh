#!/usr/bin/env bash
# Download every prebuilt Hermes CLI (darwin) from facebook/hermes GitHub
# releases, then probe each to learn which HBC bytecode version it emits.
#
# Output:
#   examples/react-native/.toolchains/hermes-<tag>/      extracted CLI
#   examples/react-native/.toolchains/manifest.tsv       hbc_version <TAB> tag <TAB> compiler_path
#
# Everything is LOCAL (the examples/ tree is gitignored). Re-running is cheap:
# already-downloaded tags are skipped.
#
# Note: published darwin binaries only go back to v0.1.0 (HBC ~59). Older HBC
# versions (40-58) have no prebuilt CLI; see build_hermesc_from_source.sh.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLDIR="$ROOT/examples/react-native/.toolchains"
MANIFEST="$TOOLDIR/manifest.tsv"
PROBE="$(mktemp -d)/probe.js"
mkdir -p "$TOOLDIR"
printf 'var x = 1 + 2; print(x);\n' > "$PROBE"

echo "Fetching release list from facebook/hermes ..."
TAGS_ASSETS="$(
  for page in 1 2 3; do
    curl -fsSL "https://api.github.com/repos/facebook/hermes/releases?per_page=100&page=$page"
  done | python3 -c '
import sys, json
seen=set()
buf=sys.stdin.read().strip()
dec=json.JSONDecoder()
i=0
while i < len(buf):
    # skip whitespace between concatenated arrays
    while i < len(buf) and buf[i].isspace(): i+=1
    if i >= len(buf): break
    try:
        data, end = dec.raw_decode(buf, i)
    except Exception:
        break
    i = end
    if not isinstance(data, list): continue
    for r in data:
        tag=r.get("tag_name")
        if not tag or tag in seen: continue
        for a in r.get("assets",[]):
            n=a["name"].lower()
            if "darwin" in n and "cli" in n:
                seen.add(tag); print(tag+"\t"+a["browser_download_url"]); break
'
)"

if [ -z "$TAGS_ASSETS" ]; then
  echo "ERROR: could not list releases (network / rate limit)." >&2
  exit 1
fi

# Fresh manifest each run (cheap to rebuild from cached extractions).
: > "$MANIFEST"

while IFS=$'\t' read -r tag url; do
  [ -z "$tag" ] && continue
  dest="$TOOLDIR/hermes-$tag"
  if [ ! -d "$dest" ]; then
    echo "Downloading $tag ..."
    tgz="$(mktemp)"
    if ! curl -fsSL "$url" -o "$tgz"; then
      echo "  WARN: download failed for $tag" >&2; continue
    fi
    mkdir -p "$dest"
    tar xzf "$tgz" -C "$dest" 2>/dev/null || { echo "  WARN: extract failed for $tag" >&2; rm -rf "$dest"; continue; }
    rm -f "$tgz"
  fi

  # Prefer hermesc; old releases only ship `hermes` (which also compiles).
  compiler="$(find "$dest" -type f -name hermesc 2>/dev/null | head -1)"
  [ -z "$compiler" ] && compiler="$(find "$dest" -type f -name hermes 2>/dev/null | head -1)"
  if [ -z "$compiler" ]; then
    echo "  WARN: no hermes/hermesc in $tag" >&2; continue
  fi
  chmod +x "$compiler" 2>/dev/null

  out="$(mktemp).hbc"
  if ! "$compiler" -emit-binary -out "$out" "$PROBE" >/dev/null 2>&1; then
    echo "  WARN: $tag failed to compile probe (incompatible binary?)" >&2; continue
  fi
  # HBC version = u32 LE at offset 8.
  ver="$(python3 -c "import sys,struct;d=open('$out','rb').read();print(struct.unpack('<I',d[8:12])[0])" 2>/dev/null)"
  rm -f "$out"
  [ -z "$ver" ] && { echo "  WARN: could not read version for $tag" >&2; continue; }
  echo "  $tag -> HBC $ver"
  printf '%s\t%s\t%s\n' "$ver" "$tag" "$compiler" >> "$MANIFEST"
done <<< "$TAGS_ASSETS"

# Keep the newest tag per HBC version (sort by version, then prefer later tag).
sort -t$'\t' -k1,1n -k2,2V "$MANIFEST" -o "$MANIFEST"
echo
echo "HBC versions available via prebuilt darwin CLI:"
cut -f1 "$MANIFEST" | sort -un | tr '\n' ' '; echo
echo "Manifest: $MANIFEST"
