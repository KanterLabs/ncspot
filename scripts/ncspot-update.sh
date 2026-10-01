# Install the latest Fedora build of ncspot from the homelab runner.
# Usage: ncspot-update [-f] [branch]   (default: main; -f reinstalls even if current)
ncspot-update() {
  local repo=KanterLabs/ncspot force= ref sha installed run conclusion began typical
  local status started done total step now elapsed pct fill bar rest dir
  [[ $1 == -f ]] && { force=1; shift; }
  ref=${1:-main}
  began=$(date +%s)
  sha=$(gh api "repos/$repo/commits/$ref" -q .sha) || return

  # 1. Already running this commit? Then there is nothing to do.
  installed=$(~/.local/bin/ncspot --version 2>/dev/null | sed -n 's/.*(\([0-9a-f]*\)).*/\1/p')
  if [[ -z $force && -n $installed && $sha == "$installed"* && -x ~/.local/bin/resonance-opentui ]]; then
    echo "ncspot is up to date (${sha:0:7} on $ref)."
    return 0
  fi
  echo "Installed: ${installed:-none}   Latest on $ref: ${sha:0:7}"

  # 2. A finished or running build of this commit? Use it rather than starting one.
  IFS='|' read -r run conclusion < <(gh run list -R "$repo" -w fedora.yml -b "$ref" -L 20 \
    --json databaseId,headSha,conclusion \
    -q "map(select(.headSha == \"$sha\" and .conclusion != \"failure\" and .conclusion != \"cancelled\"))[0] | select(.) | \"\(.databaseId)|\(.conclusion)\"")
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
  gh run download "$run" -R "$repo" -D "$dir" || { rm -rf "$dir"; return 1; }

  echo "[3/3] Installing"
  local archive checksum
  archive=$(find "$dir" -name '*.tar.gz' -print -quit)
  checksum=$(find "$dir" -name '*.sha256' -print -quit)
  if [[ -z $archive || -z $checksum ]] || ! (cd "$(dirname "$checksum")" && sha256sum -c "$(basename "$checksum")"); then
    echo "Artifact checksum verification failed; installed binaries were left alone."
    rm -rf "$dir"
    return 1
  fi
  if ! tar xzf "$archive" -C "$dir" || [[ ! -f "$dir/ncspot" || ! -f "$dir/resonance" || ! -f "$dir/resonance-opentui" ]]; then
    echo "Artifact is missing Resonance or its OpenTUI prototype; installed binaries were left alone."
    rm -rf "$dir"
    return 1
  fi
  install -Dm755 "$dir/ncspot" ~/.local/bin/ncspot &&
    install -Dm755 "$dir/resonance" ~/.local/bin/resonance &&
    install -Dm755 "$dir/resonance-opentui" ~/.local/bin/resonance-opentui &&
    now=$(date +%s) && printf 'Installed %s with OpenTUI in %d:%02d. Restart Resonance, then press F5.\n' \
      "$(~/.local/bin/ncspot --version)" $(( (now-began)/60 )) $(( (now-began)%60 ))
  local result=$?
  rm -rf "$dir"
  return "$result"
}
