"""Write the GitHub release text for a version from rust/RELEASE_NOTES.md.

usage: python rust/release/notes.py VERSION --out FILE [--run URL]

Fails unless VERSION is the workspace version in Cargo.toml and its
"## New in <label>" section is filled in. The section goes into body.md;
links relative to rust/ point at the tagged source.
"""
import argparse, pathlib, re

parser = argparse.ArgumentParser()
parser.add_argument('version')
parser.add_argument('--out', required=True, type=pathlib.Path)
parser.add_argument('--run', default='', help='the GitHub Actions run that built the release')
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parents[2]
repo = 'RamazanKara/Rubylight'
version = args.version

cargo = re.search(r'(?m)^version = "([^"]+)"', (root / 'Cargo.toml').read_text(encoding='utf-8'))[1]
if cargo != version:
    raise SystemExit(f'Cargo.toml is at {cargo}, not {version}; run rust/release/bump.py {version} first.')
match = re.search(r'-(rc\.\d+)$', version)
label = match[1] if match else version
history = (root / 'rust/RELEASE_NOTES.md').read_text(encoding='utf-8')
section = re.search(r'(?ms)^## New in %s\s*\n(.*?)(?=^## )' % re.escape(label), history)
if not section or section[1].strip() in ('', '-'):
    raise SystemExit(f"Fill in the '## New in {label}' section of rust/RELEASE_NOTES.md.")
notes = re.sub(r'\]\((?!https?:|#)([^)]+)\)',
               lambda m: f'](https://github.com/{repo}/blob/{version}/rust/{m[1]})', section[1].strip())
body = (root / 'rust/release/body.md').read_text(encoding='utf-8')
body = body.replace('{version}', version).replace('{label}', label).replace('{notes}', notes)
body = body.replace('{run}', args.run or f'https://github.com/{repo}/actions/workflows/rust-windows.yml')
args.out.write_text(body, encoding='utf-8', newline='\n')
print(f'{version}: release text written to {args.out}')
