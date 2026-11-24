#!/usr/bin/env python3
"""Package a built WASM module for Agent2API 3.5.1+ (Python 3.11+)."""
import argparse
import hashlib
import json
import os
import re
import tomllib
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

ROOT = Path(__file__).resolve().parents[1]
TARGET = 'wasm32-unknown-unknown'


def package(output, build_root):
    pack_path = ROOT / 'pack.json'
    manifest = json.loads(pack_path.read_bytes())
    permissions_path = ROOT / 'wasm.json'
    permissions = json.loads(permissions_path.read_bytes())
    cargo = tomllib.loads((ROOT / 'wasm/Cargo.toml').read_text('utf-8'))
    entry = cargo['lib']['name'] + '.wasm'
    if manifest['entry'] != {TARGET: entry} or not re.fullmatch(r'[a-z0-9_]+\.wasm', entry):
        raise SystemExit('pack.json 必须指定唯一的 WASM 入口，并与 Cargo 库名称一致')
    if (manifest['protocol'] != 1 or len(manifest['providers']) != 1 or manifest.get('ui')
            or manifest.get('signature') or permissions['abi'] != 1
            or permissions.get('verified') is not False):
        raise SystemExit('此脚本仅生成单提供商的 WASM 本地开发包')
    if manifest['version'] != cargo['package']['version']:
        raise SystemExit('pack.json 与 wasm/Cargo.toml 版本号不一致')
    for value in (manifest['id'], manifest['version']):
        if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', value):
            raise SystemExit('包 ID 或版本号无效')
    wasm = build_root / TARGET / 'release' / entry
    module = wasm.read_bytes()
    if not module.startswith(b'\0asm\1\0\0\0'):
        raise SystemExit(f'{wasm} 的 WASM 模块头无效')

    files = [('pack.json', pack_path.read_bytes()), ('wasm.json', permissions_path.read_bytes()),
             (entry, module), ('licenses/LICENSE', (ROOT / 'LICENSE').read_bytes())]
    notices = ROOT / 'THIRD-PARTY-NOTICES'
    if notices.is_file():
        files.append(('licenses/THIRD-PARTY-NOTICES', notices.read_bytes()))
    output.mkdir(parents=True, exist_ok=True)
    path = output / f"{manifest['id']}-{manifest['version']}-{TARGET}.zip"
    with ZipFile(path, 'w') as archive:
        for name, data in files:
            archive.writestr(ZipInfo(name), data, compress_type=ZIP_DEFLATED, compresslevel=9)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    path.with_suffix('.zip.sha256').write_text(f'{digest}  {path.name}\n', encoding='ascii')
    print(path)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'dist')
    parser.add_argument('--build-root', type=Path, default=Path(os.getenv('CARGO_TARGET_DIR') or ROOT / 'wasm/target'))
    args = parser.parse_args()
    package(args.output, args.build_root)
