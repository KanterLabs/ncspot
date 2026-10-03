#!/usr/bin/env python3
"""Offline end-to-end fixtures for the Resonance updater.

The fixtures deliberately exercise the failure modes that are easy to miss when
testing an updater against a real GitHub workflow:

* the repository target and version probe must use Resonance (the ncspot alias
  is intentionally given a different commit in the test fixtures);
* the legacy entry point must source and forward to the new implementation from
  an arbitrary working directory;
* config, cache, data, state, custom info paths, and OpenTUI preferences must
  be present in a verified pre-update snapshot;
* a bad checksum or an archive without the frontend must leave every installed
  executable byte-for-byte unchanged;
* an up-to-date Resonance installation must avoid workflow/download activity;
* a rollback snapshot must contain hash-verifiable copies while the active
  mutable data remains in place (rollback does not restore old data).

No network, Rust build, Bun build, real HOME, or real workflow is used.  A fake
``gh`` executable serves fixture archives and records sanitized arguments.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import shlex
import stat
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


LATEST_SHA = "a" * 40
OLD_SHA = "b" * 40


def fail(message: str) -> None:
    raise AssertionError(message)


def check(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def fake_gh(path: Path) -> None:
    """Install a fake gh that responds only to the updater's read/download API."""

    write_executable(
        path,
        r'''#!/usr/bin/env python3
import os
import shutil
import sys
from pathlib import Path

args = sys.argv[1:]
log = Path(os.environ["FAKE_GH_LOG"])
with log.open("a") as handle:
    handle.write(" ".join(args) + "\n")

if args[:1] == ["api"]:
    # This assertion is intentionally in the fake command: an updater that
    # silently keeps the old repository target fails the fixture immediately.
    if not any("repos/KanterLabs/resonance/commits/" in arg for arg in args):
        raise SystemExit("fake gh: updater used the legacy repository target")
    print(os.environ["FAKE_SHA"])
    raise SystemExit(0)

if args[:2] == ["run", "list"]:
    # The first list asks whether a successful build already exists.  Returning
    # one avoids dispatching or polling a real workflow in every install case.
    if "databaseId,headSha,conclusion" in args:
        print("123|success")
    elif "databaseId,headSha" in args:
        print("123")
    raise SystemExit(0)

if args[:2] == ["run", "download"]:
    destination = Path(args[args.index("-D") + 1])
    source = Path(os.environ["FAKE_ARTIFACT_DIR"])
    destination.mkdir(parents=True, exist_ok=True)
    for item in source.iterdir():
        if item.is_file():
            shutil.copy2(item, destination / item.name)
    raise SystemExit(0)

if args[:2] == ["workflow", "run"]:
    raise SystemExit("fake gh: unexpected workflow dispatch")

raise SystemExit(f"fake gh: unsupported invocation: {args!r}")
''',
    )


def fake_binary(path: Path, name: str, sha: str, *, info_failure: bool = False) -> None:
    if name == "resonance-opentui":
        body = f"#!/bin/sh\necho old frontend {sha}\n"
    else:
        body = f'''#!/bin/sh
if [ "${{1-}}" = "--version" ]; then
  printf '%s\\n' '{name} 1.0 ({sha})'
elif [ "${{1-}}" = "info" ]; then
  {"exit 17" if info_failure else "true"}
  printf 'USER_CONFIGURATION_PATH %s\\n' "$FAKE_CONFIG_PATH"
  printf 'USER_CACHE_PATH %s\\n' "$FAKE_CACHE_PATH"
  printf 'USER_DATA_PATH %s\\n' "$FAKE_DATA_PATH"
  printf 'USER_STATE_PATH %s\\n' "$FAKE_STATE_PATH"
else
  printf '%s\\n' '{name} fixture'
fi
'''
    write_executable(path, body)


def make_archive(directory: Path, *, include_frontend: bool, checksum_ok: bool) -> None:
    stage = directory / "stage"
    stage.mkdir(parents=True)
    for name in ("resonance", "ncspot"):
        write_executable(
            stage / name,
            f'''#!/bin/sh
if [ "${{1-}}" = "--version" ]; then
  printf '%s\\n' '{name} 2.0 ({LATEST_SHA})'
elif [ "${{1-}}" = "info" ]; then
  printf 'USER_CONFIGURATION_PATH %s\\n' "$FAKE_CONFIG_PATH"
  printf 'USER_CACHE_PATH %s\\n' "$FAKE_CACHE_PATH"
  printf 'USER_DATA_PATH %s\\n' "$FAKE_DATA_PATH"
  printf 'USER_STATE_PATH %s\\n' "$FAKE_STATE_PATH"
else
  printf '%s\\n' '{name} new fixture'
fi
''',
        )
    if include_frontend:
        write_executable(stage / "resonance-opentui", "#!/bin/sh\necho new frontend\n")

    archive = directory / "resonance.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        for item in sorted(stage.iterdir()):
            tar.add(item, arcname=item.name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if not checksum_ok:
        digest = "0" * 64 if digest != "0" * 64 else "1" * 64
    (directory / "resonance.tar.gz.sha256").write_text(f"{digest}  resonance.tar.gz\n")


def run_update(
    repo: Path,
    home: Path,
    fake_bin: Path,
    *,
    source: str,
    fixture: Path,
    sha: str = LATEST_SHA,
    force: bool = False,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    command = (
        f"source {shlex.quote(source)}; "
        f"{'resonance-update -f' if force else 'resonance-update'}"
    )
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "PATH": f"{fake_bin}:{env.get('PATH', '')}",
            "FAKE_GH_LOG": str(home / "fake-gh.log"),
            "FAKE_ARTIFACT_DIR": str(fixture),
            "FAKE_SHA": sha,
            "FAKE_CONFIG_PATH": str(home / "custom-config"),
            "FAKE_CACHE_PATH": str(home / "custom-cache"),
            "FAKE_DATA_PATH": str(home / "custom-data"),
            "FAKE_STATE_PATH": str(home / "custom-state"),
            "XDG_CONFIG_HOME": str(home / "xdg-config"),
            "XDG_CACHE_HOME": str(home / "xdg-cache"),
            "XDG_DATA_HOME": str(home / "xdg-data"),
            "XDG_STATE_HOME": str(home / "xdg-state"),
        }
    )
    return subprocess.run(
        ["bash", "-c", command],
        cwd=cwd or repo,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def seed_home(
    home: Path,
    *,
    resonance_sha: str = OLD_SHA,
    ncspot_sha: str = OLD_SHA,
    info_failure: bool = False,
    symlink_config_state: bool = False,
    symlink_primary_binary: bool = False,
) -> dict[str, bytes]:
    bin_dir = home / ".local/bin"
    fake_binary(bin_dir / "resonance", "resonance", resonance_sha, info_failure=info_failure)
    fake_binary(bin_dir / "ncspot", "ncspot", ncspot_sha)
    fake_binary(bin_dir / "resonance-opentui", "resonance-opentui", OLD_SHA)

    paths = {
        "config": home / "custom-config",
        "cache": home / "custom-cache",
        "data": home / "custom-data",
        "state": home / "custom-state",
        "prefs": home / "xdg-config/resonance/opentui-theme.json",
        "default_config": home / "xdg-config/resonance/config.toml",
        "default_cache": home / "xdg-cache/resonance/library.db",
        "default_data": home / "xdg-data/resonance/library.db",
        "default_state": home / "xdg-state/resonance/userstate.cbor",
    }
    payloads = {
        "config": b"spotify_client_id = 'fixture-client'\n",
        "cache": b"library-track-count=17\n",
        "data": b"saved-playlists=3\n",
        "state": b"queue-current-track=fixture-track\n",
        "prefs": b'{"version":1,"reducedMotion":true,"layout":"wide"}\n',
        "default_config": b"shuffle = true\n",
        "default_cache": b"cached-track=fixture-default\n",
        "default_data": b"playlist-count=3\n",
        "default_state": b"volume=42\n",
    }
    for key, path in paths.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payloads[key])
    if symlink_config_state:
        for key in ("config", "state"):
            link = paths[key]
            target = home / f"external-{key}-target"
            target.write_bytes(payloads[key])
            link.unlink()
            link.symlink_to(target)
            paths[key] = target
        cache_link = paths["cache"]
        cache_target = home / "external-cache-target"
        cache_target.mkdir()
        cache_file = cache_target / "metadata.db"
        cache_file.write_bytes(payloads["cache"])
        cache_link.unlink()
        cache_link.symlink_to(cache_target, target_is_directory=True)
        paths["cache"] = cache_file
    if symlink_primary_binary:
        primary_link = bin_dir / "resonance"
        primary_target = home / "external-resonance-binary"
        primary_target.write_bytes(primary_link.read_bytes())
        primary_target.chmod(primary_target.stat().st_mode | stat.S_IXUSR)
        primary_link.unlink()
        primary_link.symlink_to(primary_target)
    return {str(path): payloads[key] for key, path in paths.items()}


def installed_hashes(home: Path) -> dict[str, str]:
    return {
        name: hashlib.sha256((home / ".local/bin" / name).read_bytes()).hexdigest()
        for name in ("resonance", "ncspot", "resonance-opentui")
    }


def assert_snapshot(home: Path, seeded: dict[str, bytes]) -> Path:
    snapshots = sorted((home / "xdg-data/resonance/rollback").glob("pre-update-*"))
    check(len(snapshots) == 1, f"expected one rollback snapshot, got {snapshots}")
    root = snapshots[0]
    manifest_path = root / "manifest.json"
    check(manifest_path.is_file(), "rollback manifest is missing")
    manifest = json.loads(manifest_path.read_text())
    entries = {entry["source"]: entry for entry in manifest["files"]}
    for source, payload in seeded.items():
        check(source in entries, f"snapshot omitted mutable path {source}")
        entry = entries[source]
        copied = root / entry["path"]
        check(copied.read_bytes() == payload, f"snapshot payload mismatch for {source}")
        expected_mode = 0o700 if "/.local/bin/" in source else 0o600
        check(copied.stat().st_mode & 0o777 == expected_mode, f"snapshot mode is wrong for {source}")
        for parent in copied.relative_to(root).parents:
            if parent != Path("."):
                check((root / parent).stat().st_mode & 0o777 == 0o700, f"snapshot directory mode is broad for {source}")
        check(
            hashlib.sha256(copied.read_bytes()).hexdigest() == entry["sha256"],
            f"snapshot hash mismatch for {source}",
        )
    return root


def main() -> int:
    repo = Path(__file__).resolve().parents[1]
    report: dict[str, object] = {"failure_modes": 7, "cases": []}
    with tempfile.TemporaryDirectory(prefix="resonance-update-e2e-") as temporary:
        root = Path(temporary)
        fake_bin = root / "bin"
        fake_bin.mkdir()
        fake_gh(fake_bin / "gh")

        # Full install: custom paths come from `resonance info`, while the
        # frontend preference comes from the normal XDG config path.
        home = root / "full-home"
        home.mkdir()
        seeded = seed_home(home)
        credential = home / "xdg-cache/resonance/librespot-playback-fixture/credentials.json"
        credential.parent.mkdir(parents=True, exist_ok=True)
        credential.write_bytes(b'{"username":"fixture"}\n')
        seeded[str(credential)] = credential.read_bytes()
        artwork = home / "xdg-cache/resonance/covers/recoverable-artwork.bin"
        artwork.parent.mkdir(parents=True, exist_ok=True)
        artwork.write_bytes(b"re-downloadable artwork\n")
        audio = home / "xdg-cache/resonance/librespot-playback-fixture/files/stream.audio"
        audio.parent.mkdir(parents=True, exist_ok=True)
        audio.write_bytes(b"re-downloadable stream\n")
        fixture = root / "good-artifact"
        fixture.mkdir()
        make_archive(fixture, include_frontend=True, checksum_ok=True)
        result = run_update(repo, home, fake_bin, source=str(repo / "scripts/resonance-update.sh"), fixture=fixture)
        check(result.returncode == 0, f"full update failed:\n{result.stdout}\n{result.stderr}")
        check("KanterLabs/resonance" in (home / "fake-gh.log").read_text(), "new repo target was not used")
        snapshot = assert_snapshot(home, seeded)
        snapshot_sources = [entry["source"] for entry in json.loads((snapshot / "manifest.json").read_text())["files"]]
        check(str(artwork) not in snapshot_sources, "artwork cache was copied into rollback snapshot")
        check(str(audio) not in snapshot_sources, "streaming audio cache was copied into rollback snapshot")
        for name in ("resonance", "ncspot", "resonance-opentui"):
            installed = home / ".local/bin" / name
            check(installed.is_file() and os.access(installed, os.X_OK), f"required binary is not executable: {name}")
        rollback_entries = {
            Path(entry["source"]).name: snapshot / entry["path"]
            for entry in json.loads((snapshot / "manifest.json").read_text())["files"]
            if "/.local/bin/" in entry["source"]
        }
        for name in ("resonance", "ncspot", "resonance-opentui"):
            check(name in rollback_entries, f"rollback snapshot omitted {name}")
            rollback_probe = subprocess.run(
                [str(rollback_entries[name]), "--version"],
                text=True,
                capture_output=True,
                check=False,
            )
            check(rollback_probe.returncode == 0, f"rollback executable is not directly usable: {name}")
            if name != "resonance-opentui":
                check(OLD_SHA in rollback_probe.stdout, f"rollback version content is wrong: {name}")
        check((home / "custom-data").read_bytes() == seeded[str(home / "custom-data")], "data was modified")
        check((home / "custom-state").read_bytes() == seeded[str(home / "custom-state")], "state was modified")
        check((snapshot / "manifest.json").stat().st_mode & 0o777 == 0o600, "manifest permissions are broad")
        report["cases"].append("full-install-and-verified-snapshot")

        # An installed executable may itself be a symlink into custom storage.
        # The resolved retained copy must still be directly executable after
        # the update, while mutable data keeps the stricter file mode.
        binary_symlink_home = root / "binary-symlink-home"
        binary_symlink_home.mkdir()
        seed_home(binary_symlink_home, symlink_primary_binary=True)
        binary_symlink_fixture = root / "binary-symlink-artifact"
        binary_symlink_fixture.mkdir()
        make_archive(binary_symlink_fixture, include_frontend=True, checksum_ok=True)
        result = run_update(
            repo,
            binary_symlink_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=binary_symlink_fixture,
        )
        check(result.returncode == 0, f"symlinked executable update failed:\n{result.stdout}\n{result.stderr}")
        binary_snapshot = sorted((binary_symlink_home / "xdg-data/resonance/rollback").glob("pre-update-*"))[0]
        binary_manifest = json.loads((binary_snapshot / "manifest.json").read_text())["files"]
        binary_target = binary_symlink_home / "external-resonance-binary"
        binary_entry = next((entry for entry in binary_manifest if entry["source"] == str(binary_target)), None)
        check(binary_entry is not None, "resolved symlinked executable was omitted from snapshot")
        binary_probe = subprocess.run(
            [str(binary_snapshot / binary_entry["path"]), "--version"],
            text=True,
            capture_output=True,
            check=False,
        )
        check(binary_probe.returncode == 0 and OLD_SHA in binary_probe.stdout, "resolved symlinked executable is not directly usable")
        check((binary_snapshot / binary_entry["path"]).stat().st_mode & 0o777 == 0o700, "resolved executable snapshot mode is not 0700")
        report["cases"].append("symlinked-executable-rollback")

        # A primary binary that cannot report its effective paths must fail
        # closed.  Falling back to guessed roots could omit a custom config or
        # state path and then install with an unverifiable rollback snapshot.
        info_failure_home = root / "info-failure-home"
        info_failure_home.mkdir()
        seed_home(info_failure_home, info_failure=True)
        before = installed_hashes(info_failure_home)
        info_failure_fixture = root / "info-failure-artifact"
        info_failure_fixture.mkdir()
        make_archive(info_failure_fixture, include_frontend=True, checksum_ok=True)
        result = run_update(
            repo,
            info_failure_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=info_failure_fixture,
            force=True,
        )
        check(result.returncode != 0, "failed resonance info unexpectedly allowed installation")
        check(installed_hashes(info_failure_home) == before, "failed resonance info replaced an executable")
        check(
            not (info_failure_home / "xdg-data/resonance/rollback").exists(),
            "failed resonance info left a misleading rollback snapshot",
        )
        report["cases"].append("info-failure-is-closed")

        # Symlinked config/state files are valid user layouts.  Snapshot the
        # resolved content once rather than silently dropping the link target.
        symlink_home = root / "symlink-home"
        symlink_home.mkdir()
        symlinked = seed_home(symlink_home, symlink_config_state=True)
        symlink_fixture = root / "symlink-artifact"
        symlink_fixture.mkdir()
        make_archive(symlink_fixture, include_frontend=True, checksum_ok=True)
        result = run_update(
            repo,
            symlink_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=symlink_fixture,
        )
        check(result.returncode == 0, f"symlinked mutable paths failed:\n{result.stdout}\n{result.stderr}")
        symlink_snapshot = assert_snapshot(symlink_home, symlinked)
        symlink_manifest = json.loads((symlink_snapshot / "manifest.json").read_text())["files"]
        symlink_sources = [entry["source"] for entry in symlink_manifest]
        check(str(symlink_home / "custom-config") not in symlink_sources, "symlink path was duplicated in snapshot")
        check(str(symlink_home / "custom-state") not in symlink_sources, "symlink state path was duplicated in snapshot")
        check(str(symlink_home / "custom-cache") not in symlink_sources, "symlink cache root was duplicated in snapshot")
        for source in symlinked:
            if "external-" in source:
                check(symlink_sources.count(source) == 1, f"symlink target was not snapshotted once: {source}")
        report["cases"].append("symlinked-mutable-paths")

        # The old path is a sourceable forwarding alias and must work outside
        # the checkout directory.  It also proves the alias reaches the same
        # repo target and installation behavior.
        alias_home = root / "alias-home"
        alias_home.mkdir()
        seed_home(alias_home)
        alias_fixture = root / "alias-artifact"
        alias_fixture.mkdir()
        make_archive(alias_fixture, include_frontend=True, checksum_ok=True)
        result = run_update(
            repo,
            alias_home,
            fake_bin,
            source=str(repo / "scripts/ncspot-update.sh"),
            fixture=alias_fixture,
            cwd=alias_home,
        )
        check(result.returncode == 0, f"legacy alias failed:\n{result.stdout}\n{result.stderr}")
        report["cases"].append("legacy-source-forwarding")

        # The primary Resonance executable controls the up-to-date decision;
        # ncspot deliberately claims the latest commit to catch a stale probe.
        probe_home = root / "probe-home"
        probe_home.mkdir()
        seed_home(probe_home, resonance_sha=OLD_SHA, ncspot_sha=LATEST_SHA)
        probe_fixture = root / "probe-artifact"
        probe_fixture.mkdir()
        make_archive(probe_fixture, include_frontend=True, checksum_ok=True)
        result = run_update(
            repo,
            probe_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=probe_fixture,
        )
        check(result.returncode == 0, f"primary version probe failed:\n{result.stdout}\n{result.stderr}")
        check("new fixture" in (probe_home / ".local/bin/resonance").read_text(), "Resonance was incorrectly treated as current")
        report["cases"].append("primary-resonance-version-probe")

        # A bad checksum must not replace any executable.
        bad_home = root / "bad-checksum-home"
        bad_home.mkdir()
        seed_home(bad_home)
        before = installed_hashes(bad_home)
        bad_fixture = root / "bad-checksum-artifact"
        bad_fixture.mkdir()
        make_archive(bad_fixture, include_frontend=True, checksum_ok=False)
        result = run_update(
            repo,
            bad_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=bad_fixture,
            force=True,
        )
        check(result.returncode != 0, "bad checksum unexpectedly succeeded")
        check(installed_hashes(bad_home) == before, "bad checksum replaced an executable")
        report["cases"].append("invalid-checksum-is-non-destructive")

        # A valid archive missing the frontend has the same non-destructive
        # guarantee as a checksum failure.
        missing_home = root / "missing-frontend-home"
        missing_home.mkdir()
        seed_home(missing_home)
        before = installed_hashes(missing_home)
        missing_fixture = root / "missing-frontend-artifact"
        missing_fixture.mkdir()
        make_archive(missing_fixture, include_frontend=False, checksum_ok=True)
        result = run_update(
            repo,
            missing_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=missing_fixture,
            force=True,
        )
        check(result.returncode != 0, "missing frontend unexpectedly succeeded")
        check(installed_hashes(missing_home) == before, "missing frontend replaced an executable")
        report["cases"].append("missing-frontend-is-non-destructive")

        # Matching Resonance commit plus frontend takes the no-op path and must
        # avoid downloading or dispatching a workflow.
        current_home = root / "current-home"
        current_home.mkdir()
        seed_home(current_home, resonance_sha=LATEST_SHA, ncspot_sha=OLD_SHA)
        current_fixture = root / "current-artifact"
        current_fixture.mkdir()
        make_archive(current_fixture, include_frontend=True, checksum_ok=True)
        result = run_update(
            repo,
            current_home,
            fake_bin,
            source=str(repo / "scripts/resonance-update.sh"),
            fixture=current_fixture,
        )
        check(result.returncode == 0, f"up-to-date shortcut failed:\n{result.stdout}\n{result.stderr}")
        check("run download" not in (current_home / "fake-gh.log").read_text(), "up-to-date path downloaded an artifact")
        check("workflow run" not in (current_home / "fake-gh.log").read_text(), "up-to-date path dispatched a workflow")
        report["cases"].append("up-to-date-shortcut")

        report["status"] = "passed"
        report["temporary_root"] = "<fixture-root>"
        report_path = Path(tempfile.gettempdir()) / "resonance-update-e2e-report.json"
        report_path.write_text(json.dumps(report, indent=2) + "\n")
        print(f"resonance updater E2E passed: {len(report['cases'])} cases")
        print(f"sanitized report: {report_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"resonance updater E2E failed: {error}", file=sys.stderr)
        raise SystemExit(1)
