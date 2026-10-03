# Install the latest Fedora build of Resonance from the homelab runner.
# Usage: resonance-update [-f] [branch]   (default: main; -f reinstalls even if current)
#
# This file is intentionally sourceable: it defines resonance-update without
# changing the caller's shell options or working directory.  ncspot-update.sh
# is the compatibility entry point for existing dotfiles.
resonance-update() {
  local repo=KanterLabs/resonance force= ref sha installed run conclusion began typical
  local status started done total step now elapsed pct fill bar rest dir
  local updater_home bin_dir
  [[ ${1-} == -f ]] && { force=1; shift; }
  ref=${1:-main}
  updater_home=${HOME:?HOME must be set}
  bin_dir="$updater_home/.local/bin"
  began=$(date +%s)
  sha=$(gh api "repos/$repo/commits/$ref" -q .sha) || return

  # 1. Already running this commit? Then there is nothing to do.  Resonance is
  # authoritative; the ncspot executable is only retained as a compatibility
  # alias and can be older or absent.
  installed=$("$bin_dir/resonance" --version 2>/dev/null | sed -n 's/.*(\([0-9a-f]\{7,64\}\)).*/\1/p')
  if [[ -z $force && -n $installed && $sha == "$installed"* && -x "$bin_dir/resonance-opentui" ]]; then
    echo "Resonance is up to date (${sha:0:7} on $ref)."
    return 0
  fi
  echo "Installed: ${installed:-none}   Latest on $ref: ${sha:0:7}"

  # 2. A finished or running build of this commit? Use it rather than starting one.
  IFS='|' read -r run conclusion < <(gh run list -R "$repo" -w fedora.yml -b "$ref" -L 20 \
    --json databaseId,headSha,conclusion \
    -q "map(select(.headSha == \"$sha\" and .conclusion != \"failure\" and .conclusion != \"cancelled\"))[0] | select(.) | \"\\(.databaseId)|\\(.conclusion)\"")
  if [[ $conclusion == success ]]; then
    echo "A build of ${sha:0:7} is ready, so there's nothing to build."
  else
    # 3. Otherwise build it, with progress.
    if [[ -z $run ]]; then
      gh workflow run fedora.yml -R "$repo" --ref "$ref" || return
      printf 'Starting a build'
      until run=$(gh run list -R "$repo" -w fedora.yml -b "$ref" -e workflow_dispatch -L 5 \
          --json databaseId,headSha -q "map(select(.headSha == \"$sha\"))[0].databaseId // empty") \
          && [[ -n $run ]]; do printf '.'; sleep 3; done
      echo
    else
      echo "A build of ${sha:0:7} is already running; following it."
    fi

    typical=$(gh run list -R "$repo" -w fedora.yml -s success -L 1 --json startedAt,updatedAt \
      -q '.[0] | [.startedAt, .updatedAt] | join(" ")' 2>/dev/null |
      { read -r a b && [[ -n $a ]] && echo $(( $(date -d "$b" +%s) - $(date -d "$a" +%s) )); })

    echo "[1/3] Building ${sha:0:7} on the homelab runner"
    while :; do
      IFS='|' read -r status conclusion started done total step < <(
        gh run view "$run" -R "$repo" --json status,conclusion,startedAt,jobs -q '
          (.jobs[0].steps // []) as $s
          | [.status, .conclusion, .startedAt,
             ($s | map(select(.status == "completed")) | length),
             ($s | length),
             ($s | map(select(.status == "in_progress")) | .[0].name // "")]
          | map(tostring) | join("|")')
      [[ $status == completed ]] && break
      now=$(date +%s)
      elapsed=$(( now - $(date -d "$started" +%s) ))
      if [[ $status != in_progress || $total == 0 ]]; then
        printf '\r\033[K  waiting for a runner... %d:%02d' $((elapsed/60)) $((elapsed%60))
      else
        if [[ -n $typical && $typical -gt 0 ]]; then
          pct=$(( elapsed * 100 / typical )); (( pct > 99 )) && pct=99
        else
          pct=$(( done * 100 / total ))
        fi
        fill=$(( pct * 30 / 100 ))
        printf -v bar '%*s' "$fill" ''; bar=${bar// /█}
        printf -v rest '%*s' $((30 - fill)) ''; bar+=${rest// /░}
        printf '\r\033[K  [%s] %3d%%  %d:%02d%s  step %d/%d: %s' "$bar" "$pct" \
          $((elapsed/60)) $((elapsed%60)) \
          "${typical:+ / ~$((typical/60)):$(printf %02d $((typical%60)))}" \
          $((done + 1)) "$total" "$step"
      fi
      sleep 5
    done
    echo
    if [[ $conclusion != success ]]; then
      echo "The build ended with: $conclusion. See: gh run view $run -R $repo --log-failed"
      return 1
    fi
  fi

  echo "[2/3] Downloading"
  dir=$(mktemp -d)
  if ! gh run download "$run" -R "$repo" -D "$dir"; then
    rm -rf "$dir"
    return 1
  fi

  echo "[3/3] Installing"
  local archive checksum extract
  archive=$(find "$dir" -type f -name '*.tar.gz' -print -quit)
  checksum=$(find "$dir" -type f -name '*.sha256' -print -quit)
  if [[ -z $archive || -z $checksum ]] || ! (cd "$(dirname "$checksum")" && sha256sum -c "$(basename "$checksum")"); then
    echo "Artifact checksum verification failed; installed binaries were left alone."
    rm -rf "$dir"
    return 1
  fi

  # Extract into a disposable directory only after checksum verification.  The
  # member check rejects absolute and parent-traversal names before tar can
  # write outside the staging directory.
  extract="$dir/extract"
  mkdir "$extract"
  while IFS= read -r member; do
    case $member in
      /*|../*|*/../*|*/..)
        echo "Artifact contains an unsafe path; installed binaries were left alone."
        rm -rf "$dir"
        return 1
        ;;
    esac
  done < <(tar tzf "$archive") || {
    echo "Artifact archive could not be inspected; installed binaries were left alone."
    rm -rf "$dir"
    return 1
  }
  if ! tar xzf "$archive" -C "$extract" || [[ ! -f "$extract/ncspot" || -L "$extract/ncspot" || ! -f "$extract/resonance" || -L "$extract/resonance" || ! -f "$extract/resonance-opentui" || -L "$extract/resonance-opentui" ]]; then
    echo "Artifact is missing Resonance or its OpenTUI interface; installed binaries were left alone."
    rm -rf "$dir"
    return 1
  fi

  # Retain rollback executables and every discovered mutable file before
  # replacing anything.  The roots include platform/XDG defaults and paths
  # reported by Resonance info, so custom cache/config/data/state layouts and
  # frontend preferences remain covered.
  if ! python3 - "$updater_home" "$bin_dir" <<'RESONANCE_BACKUP_PY'
import hashlib
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

home = Path(sys.argv[1]).resolve()
bin_dir = Path(sys.argv[2]).resolve()

def absolute(value):
    value = value.strip()
    if not value or value == "not found":
        return None
    path = Path(value).expanduser()
    return path if path.is_absolute() else None

def xdg_root(variable, fallback):
    value = os.environ.get(variable, "").strip()
    if value:
        candidate = Path(value).expanduser()
        if candidate.is_absolute():
            return candidate
    return home / fallback

def default_roots():
    # Linux/XDG is the supported desktop layout.  The macOS fallback mirrors
    # platform-dirs so info remains the authority when it chooses another path.
    if sys.platform == "darwin":
        return {
            "config": [home / "Library/Application Support" / name for name in ("resonance", "ncspot")],
            "cache": [home / "Library/Caches" / name for name in ("resonance", "ncspot")],
            "data": [home / "Library/Application Support" / name for name in ("resonance", "ncspot")],
            "state": [home / "Library/Preferences" / name for name in ("resonance", "ncspot")],
        }
    return {
        "config": [xdg_root("XDG_CONFIG_HOME", ".config") / name for name in ("resonance", "ncspot")],
        "cache": [xdg_root("XDG_CACHE_HOME", ".cache") / name for name in ("resonance", "ncspot")],
        "data": [xdg_root("XDG_DATA_HOME", ".local/share") / name for name in ("resonance", "ncspot")],
        "state": [xdg_root("XDG_STATE_HOME", ".local/state") / name for name in ("resonance", "ncspot")],
    }

roots = default_roots()
engine = bin_dir / "resonance"
if engine.exists():
    if not engine.is_file() or not os.access(engine, os.X_OK):
        raise RuntimeError("installed resonance cannot run info")
    info = subprocess.run([str(engine), "info"], capture_output=True, text=True, check=False)
    if info.returncode != 0:
        raise RuntimeError(f"resonance info failed with status {info.returncode}")
    for line in info.stdout.splitlines():
        key, _, value = line.partition(" ")
        path = absolute(value)
        if path is None:
            continue
        if key == "USER_CONFIGURATION_PATH": roots["config"].append(path)
        elif key == "USER_CACHE_PATH": roots["cache"].append(path)
        elif key == "USER_DATA_PATH": roots["data"].append(path)
        elif key == "USER_STATE_PATH": roots["state"].append(path)

# Keep rollback records in the canonical Resonance data area.  This remains
# stable when an existing installation reports a custom data path and avoids
# placing a record inside a file supplied as a malformed info path.
rollback_parent = xdg_root("XDG_DATA_HOME", ".local/share") / "resonance" / "rollback"
excluded_rollback = rollback_parent.resolve()
root = rollback_parent / ("pre-update-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ"))
root.mkdir(parents=True, mode=0o700)
os.chmod(root, 0o700)

def cache_file_is_recoverable(path, cache_root):
    try:
        relative = path.relative_to(cache_root)
    except ValueError:
        return True
    parts = relative.parts
    if parts and parts[0] == "covers":
        return False
    return not (
        len(parts) > 1
        and parts[0].startswith("librespot-playback-")
        and parts[1] == "files"
    )

def files_under(directory, cache=False):
    resolved = directory.resolve()
    if resolved == excluded_rollback or excluded_rollback in resolved.parents:
        return []
    if resolved.is_file():
        return [resolved]
    if not resolved.is_dir():
        return []
    files = []
    for path in resolved.rglob("*"):
        if not path.is_file():
            continue
        if cache and not cache_file_is_recoverable(path, resolved):
            continue
        files.append(path.resolve())
    return files

sources = []
for name in ("resonance", "ncspot", "resonance-opentui"):
    source = bin_dir / name
    if source.is_file():
        sources.append(source.resolve())
for kind, directories in roots.items():
    for directory in set(directories):
        sources.extend(files_under(directory, cache=kind == "cache"))

manifest = []
seen = set()
try:
    for source in sorted(sources):
        source = source.resolve()
        if source in seen or source == excluded_rollback or excluded_rollback in source.parents:
            continue
        seen.add(source)
        try:
            relative = source.relative_to(home)
        except ValueError:
            relative = Path("_external_storage") / source.relative_to(source.anchor)
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        for parent in (target.parent, *target.parent.parents):
            if parent == root or root in parent.parents:
                os.chmod(parent, 0o700)
        for attempt in range(3):
            expected = hashlib.sha256(source.read_bytes()).hexdigest()
            shutil.copy2(source, target)
            os.chmod(target, 0o600)
            actual = hashlib.sha256(target.read_bytes()).hexdigest()
            current = hashlib.sha256(source.read_bytes()).hexdigest()
            if actual == expected == current:
                break
        else:
            raise RuntimeError(f"a file changed during backup: {source}")
        manifest.append({"path": str(relative), "source": str(source), "sha256": actual, "bytes": target.stat().st_size, "mode": source.stat().st_mode & 0o777})
    record = root / "manifest.json"
    record.write_text(json.dumps({"version": 1, "files": manifest}, indent=2) + "\n")
    os.chmod(record, 0o600)
except Exception:
    shutil.rmtree(root, ignore_errors=True)
    raise

print(f"Verified rollback snapshot ({len(manifest)} files)")
RESONANCE_BACKUP_PY
  then
    echo "Rollback backup failed; installed binaries were left alone."
    rm -rf "$dir"
    return 1
  fi

  install -Dm755 "$extract/ncspot" "$bin_dir/ncspot" &&
    install -Dm755 "$extract/resonance" "$bin_dir/resonance" &&
    install -Dm755 "$extract/resonance-opentui" "$bin_dir/resonance-opentui" &&
    now=$(date +%s) && printf 'Installed %s with OpenTUI in %d:%02d. Restart Resonance to open the new workspace.\n' \
      "$("$bin_dir/resonance" --version)" $(( (now-began)/60 )) $(( (now-began)%60 ))
  local result=$?
  rm -rf "$dir"
  return "$result"
}
