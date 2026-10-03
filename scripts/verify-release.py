#!/usr/bin/env python3
"""Verify a real release bundle and isolated fresh/legacy installation paths.

Failure cases: missing frontend/assets, broken executables, alias drift, wrong
directory precedence, custom-base regressions, or mutation of populated data.
The existing Rust compatibility tests cover decoding and rollback of user state.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import zipfile


def digest_tree(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file()}


def run(executable, args, env):
    return subprocess.run([str(executable), *args], env=env, check=True,
                          capture_output=True, text=True, timeout=30).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--report', type=Path, required=True)
    parser.add_argument('--rollback', type=Path)
    args = parser.parse_args()
    args.archive = args.archive.resolve()
    with tempfile.TemporaryDirectory(prefix='resonance-release-check-') as temp:
        root = Path(temp)
        bundle = root / 'bundle'
        bundle.mkdir()
        deb = args.archive.suffix == '.deb'
        if deb:
            subprocess.run(['dpkg-deb', '-x', str(args.archive), str(bundle)], check=True)
            executable_root = bundle / 'usr/bin'
            required = ['usr/bin/resonance', 'usr/bin/ncspot', 'usr/bin/resonance-opentui',
                        'usr/share/applications/resonance.desktop',
                        'usr/share/icons/hicolor/scalable/apps/resonance.svg',
                        'usr/share/doc/resonance/README.md', 'usr/share/man/man1/resonance.1.gz',
                        'usr/share/bash-completion/completions/resonance',
                        'usr/share/doc/resonance/licenses/opentui/@opentui/core/LICENSE']
        else:
            if args.archive.suffix == '.zip':
                with zipfile.ZipFile(args.archive) as archive:
                    archive.extractall(bundle)
            else:
                with tarfile.open(args.archive) as archive:
                    archive.extractall(bundle, filter='data')
            executable_root = bundle
            required = ['resonance', 'ncspot', 'resonance-opentui', 'resonance-wezterm',
                        'resonance-update.sh', 'ncspot-update.sh',
                        'share/applications/resonance.desktop',
                        'share/icons/hicolor/scalable/apps/resonance.svg',
                        'share/doc/resonance/README.md', 'share/man/man1/resonance.1',
                        'share/bash-completion/completions/resonance', 'licenses/resonance/LICENSE']
        missing = [name for name in required if not (bundle / name).is_file()]
        if missing:
            raise SystemExit('Incomplete default Unix installation: ' + ', '.join(missing))

        home = root / 'home'
        home.mkdir()
        runtime = root / 'runtime'
        runtime.mkdir(mode=0o700)
        env = {**os.environ, 'HOME': str(home), 'XDG_CONFIG_HOME': str(home / '.config'),
               'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_DATA_HOME': str(home / '.local/share'),
               'XDG_STATE_HOME': str(home / '.local/state'), 'XDG_RUNTIME_DIR': str(runtime)}
        engine = executable_root / 'resonance'
        alias = executable_root / 'ncspot'
        version = run(engine, ['--version'], env).strip()
        assert version.startswith('resonance '), version
        assert run(alias, ['--version'], env).strip() == version
        assert 'resonance' in run(engine, ['--help'], env)
        smoke = run(executable_root / 'resonance-opentui', ['--smoke'], env).strip()
        assert 'smoke' in smoke and 'OK' in smoke, smoke
        checks = ['bundle contents', 'primary and alias identity', 'frontend smoke']

        def info(binary, extra=()):
            return dict(line.split(' ', 1) for line in run(binary, [*extra, 'info'], env).splitlines())

        fresh = info(engine)
        assert fresh['USER_CONFIGURATION_PATH'] == str(home / '.config/resonance')
        assert fresh['USER_CACHE_PATH'] == str(home / '.cache/resonance')
        checks.append('fresh installation paths')
        for name, content in {
            '.config/ncspot/config.toml': b'shuffle = true\n',
            '.config/ncspot/userstate.cbor': b'populated playback fixture',
            '.cache/ncspot/tracks.db': b'populated library fixture',
            '.cache/ncspot/librespot/credentials.json': b'{"fixture":true}',
            '.local/share/ncspot/history.json': b'{"fixture_id":"stable-1","count":7}',
            '.local/state/ncspot/session.json': b'{"fixture":true}',
            '.config/resonance/opentui-theme.json': b'{"version":1,"theme":"dark"}',
        }.items():
            path = home / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        before = digest_tree(home)
        legacy = info(engine)
        assert legacy['USER_CONFIGURATION_PATH'] == str(home / '.config/ncspot')
        assert legacy['USER_CACHE_PATH'] == str(home / '.cache/ncspot')
        assert info(alias) == legacy
        assert digest_tree(home) == before
        checks.append('populated legacy paths and data preservation through both names')
        (home / '.config/resonance/config.toml').write_text('shuffle = false\n')
        before = digest_tree(home)
        assert info(engine)['USER_CONFIGURATION_PATH'] == str(home / '.config/resonance')
        assert digest_tree(home) == before
        checks.append('both directory names retain data and prefer Resonance config')
        base = home / 'custom-base'
        assert info(engine, ['--basepath', str(base)])['USER_CONFIGURATION_PATH'] == str(base / '.config')
        assert digest_tree(home) == before
        checks.append('explicit basepath')
        if args.rollback:
            assert info(args.rollback.resolve()) == info(engine)
            assert digest_tree(home) == before
            checks.append('retained rollback executable path selection and data preservation')
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps({'archive': args.archive.name,
            'sha256': hashlib.sha256(args.archive.read_bytes()).hexdigest(),
            'version': version, 'checks': checks,
            'state_decoding': 'covered separately by Rust populated CBOR compatibility tests'}, indent=2) + '\n')
        print(json.dumps({'passed': len(checks), 'report': str(args.report)}))


if __name__ == '__main__':
    main()
