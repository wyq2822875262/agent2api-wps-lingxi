#!/usr/bin/env python3
"""Run the standalone plugin's RPC tests with a native debug build."""
import argparse
import json
import subprocess
import sys
from pathlib import Path

from package import ROOT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    manifest = json.loads((ROOT / 'scripts/legacy-pack.json').read_text('utf-8'))
    parser.add_argument('--target', required=True, choices=tuple(manifest['entry']))
    args = parser.parse_args()
    target = args.target
    exe = ROOT / 'target' / target / 'debug' / Path(manifest['entry'][target]).name
    provider = manifest['providers'][0]['id']
    tests = [(f'verify-{provider}.py', ['--target', target] if provider == 'wps-lingxi' else [])]
    # The extra QR fixture uses the OpenSSL command-line tool. Cargo unit tests
    # and the independent RPC integration suite still run on Windows.
    if provider == 'mclaw' and 'windows' not in target:
        tests.append(('verify-mclaw-qr.py', []))
    for script, options in tests:
        subprocess.run([sys.executable, str(ROOT / 'scripts' / script), '--exe', str(exe), *options],
                       cwd=ROOT, check=True)


if __name__ == '__main__':
    main()
