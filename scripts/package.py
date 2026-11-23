#!/usr/bin/env python3
"""Create an unsigned, single-platform Agent2API plugin ZIP and its checksum."""
import argparse
import hashlib
import json
import re
import struct
import tomllib
from pathlib import Path, PurePosixPath
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

ROOT = Path(__file__).resolve().parents[1]


def load_manifest():
    return json.loads((ROOT / 'pack.json').read_text(encoding='utf-8'))


def check_binary(content, target):
    """Validate the platform and CPU in the actual artifact, not just the file name."""
    x64 = target.startswith('x86_64-')
    if target.endswith('-linux-gnu'):
        return (content.startswith(b'\x7fELF\x02\x01') and len(content) >= 20
                and int.from_bytes(content[18:20], 'little') == (62 if x64 else 183))
    if target.endswith('-apple-darwin'):
        return content.startswith(bytes.fromhex('cffaedfe07000001' if x64 else 'cffaedfe0c000001'))
    if target == 'x86_64-pc-windows-msvc' and content.startswith(b'MZ') and len(content) >= 64:
        offset = struct.unpack_from('<I', content, 0x3c)[0]
        return content[offset:offset + 6] == b'PE\x00\x00\x64\x86'
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    manifest = load_manifest()
    parser.add_argument('--target', required=True, choices=tuple(manifest['entry']))
    parser.add_argument('--exe', type=Path, help='Override the default path to the release executable')
    args = parser.parse_args()
    target, pack_id, version = args.target, manifest['id'], manifest['version']
    if not re.fullmatch(r'[a-z0-9-]+', pack_id) or not re.fullmatch(r'[0-9]+(?:\.[0-9]+){2}(?:-[a-zA-Z0-9.-]+)?', version):
        parser.error('Invalid pack ID or version')
    crate = ROOT / 'crates' / pack_id.removeprefix('agent2api-') / 'Cargo.toml'
    package = tomllib.loads(crate.read_text(encoding='utf-8'))['package']
    if (pack_id, version) != (package['name'], package['version']):
        parser.error('pack.json and Cargo.toml must declare the same name and version')
    program = pack_id + ('.exe' if target.endswith('-windows-msvc') else '')
    entry = manifest['entry'][target]
    if entry != f'bin/{program}':
        parser.error(f'Unexpected manifest entry for {target}')
    binary = args.exe or ROOT / 'target' / target / 'release' / program
    content = binary.read_bytes()
    if not check_binary(content, target):
        parser.error(f'{binary} is not a {target} executable')
    manifest['entry'] = {target: entry}

    files = {'LICENSE': ROOT / 'LICENSE', 'README.md': ROOT / 'README.md'}
    for name in ('PROVENANCE.json', 'THIRD-PARTY-NOTICES'):
        source = ROOT / name
        if source.is_file():
            files[name] = source
    for panel in manifest.get('ui', []):
        panel_path = PurePosixPath(panel['entry'])
        if len(panel_path.parts) != 2 or panel_path.is_absolute() or '..' in panel_path.parts:
            parser.error('Expected a panel entry of the form directory/index.html')
        if not (ROOT / 'ui' / panel['entry']).is_file():
            parser.error(f'Missing UI entry: {panel["entry"]}')
        for source in (ROOT / 'ui' / panel_path.parts[0]).rglob('*'):
            if source.is_file() and not source.is_symlink() and not any(
                part.startswith('.') for part in source.relative_to(ROOT).parts
            ):
                files[source.relative_to(ROOT).as_posix()] = source

    output = ROOT / 'dist' / f'{pack_id}-{version}-{target}.zip'
    output.parent.mkdir(exist_ok=True)
    with ZipFile(output, 'w') as archive:
        def add(name, data, mode=0o644):
            info = ZipInfo(name)
            info.create_system = 3
            info.external_attr = (0o100000 | mode) << 16
            archive.writestr(info, data, compress_type=ZIP_DEFLATED)

        add('pack.json', (json.dumps(manifest, ensure_ascii=False, indent=2) + '\n').encode())
        add(entry, content, 0o755)
        for name, path in sorted(files.items()):
            add(name, path.read_bytes())
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    output.with_suffix('.zip.sha256').write_text(f'{digest}  {output.name}\n', encoding='ascii', newline='\n')
    print(f'{output}\nsha256={digest}')


if __name__ == '__main__':
    main()
