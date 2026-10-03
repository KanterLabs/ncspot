#!/usr/bin/env python3
"""Assemble a complete native Resonance release from already-built artifacts."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def copy(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)


def notices(destination):
    copy(ROOT / 'LICENSE', destination / 'resonance/LICENSE')
    modules = ROOT / 'prototype/opentui/node_modules'
    for path in modules.rglob('*'):
        if path.is_file() and path.name.lower().startswith(('license', 'copying', 'notice', 'patents')):
            copy(path, destination / 'opentui' / path.relative_to(modules))
    for name in ['LICENSES.md', 'THIRD_PARTY_LICENSES.md', 'THIRD_PARTY_NOTICES.md']:
        path = ROOT / 'prototype/opentui' / name
        if path.exists():
            copy(path, destination / 'opentui' / name)
    if not any((destination / 'opentui').rglob('*')):
        raise SystemExit('OpenTUI dependency notices are missing; install frontend dependencies first')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', default='release')
    parser.add_argument('--target', help='Cargo target triple; omit for a native Cargo build')
    parser.add_argument('--platform', required=True, help='Release filename suffix, e.g. fedora43-x86_64')
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, default=ROOT / 'dist')
    parser.add_argument('--notices-only', type=Path, help='Stage notices for a Debian package')
    args = parser.parse_args()
    if args.notices_only:
        notices(args.notices_only)
        return
    for value in [args.platform, args.version]:
        if not value or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-' for c in value):
            parser.error('platform and version must be safe filename components')
    build = ROOT / 'target'
    if args.target:
        build /= args.target
    build /= args.profile
    windows = bool(args.target and 'windows' in args.target)
    suffix = '.exe' if windows else ''
    args.output.mkdir(parents=True, exist_ok=True)
    name = f'resonance-{args.version}-{args.platform}'
    with tempfile.TemporaryDirectory(prefix='resonance-package-') as temp:
        stage = Path(temp)
        for binary in ['resonance', 'ncspot']:
            copy(build / (binary + suffix), stage / (binary + suffix))
        copy(ROOT / 'README.md', stage / 'share/doc/resonance/README.md')
        copy(ROOT / 'LICENSE', stage / 'licenses/resonance/LICENSE')
        if not windows:
            frontend = ROOT / 'prototype/opentui/dist/resonance-opentui'
            subprocess.run([str(frontend), '--smoke'], check=True)
            copy(frontend, stage / 'resonance-opentui')
            for helper in ['resonance-wezterm', 'resonance-update.sh', 'ncspot-update.sh']:
                copy(ROOT / 'scripts' / helper, stage / helper)
            copy(ROOT / 'misc/resonance.desktop', stage / 'share/applications/resonance.desktop')
            copy(ROOT / 'images/resonance.svg', stage / 'share/icons/hicolor/scalable/apps/resonance.svg')
            copy(ROOT / 'misc/resonance.1', stage / 'share/man/man1/resonance.1')
            for source, destination in {
                'resonance.bash': 'bash-completion/completions/resonance',
                '_resonance': 'zsh/site-functions/_resonance',
                'resonance.fish': 'fish/vendor_completions.d/resonance.fish',
                'resonance.elv': 'elvish/lib/resonance.elv',
                '_resonance.ps1': 'powershell/completions/_resonance.ps1',
            }.items():
                copy(ROOT / 'misc' / source, stage / 'share' / destination)
            notices(stage / 'licenses')
        (stage / 'release.json').write_text(json.dumps({
            'product': 'Resonance', 'version': args.version, 'platform': args.platform,
            'interface': 'legacy Cursive' if windows else 'OpenTUI',
            'files': {p.relative_to(stage).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                      for p in sorted(stage.rglob('*')) if p.is_file()},
        }, indent=2) + '\n')
        if windows:
            archive = args.output / (name + '.zip')
            with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as output:
                for path in sorted(stage.rglob('*')):
                    if path.is_file():
                        output.write(path, path.relative_to(stage))
        else:
            archive = args.output / (name + '.tar.gz')
            with tarfile.open(archive, 'w:gz') as output:
                for path in sorted(stage.iterdir()):
                    output.add(path, arcname=path.name)
        checksum = args.output / (name + '.sha256')
        checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
        print(json.dumps({'archive': str(archive), 'checksum': str(checksum)}))


if __name__ == '__main__':
    main()
