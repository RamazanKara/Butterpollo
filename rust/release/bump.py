"""Bump the release version and open its release notes section.

Writes LF line endings on any platform.

usage: python rust/release/bump.py 2.0.0-rc.14 [--notes FILE]

Updates the workspace version (Cargo.toml and the workspace crates in
Cargo.lock); the full version, "Download rc.N" and "For rc.N," in the
READMEs and docs; the title, history and installer names in
rust/RELEASE_NOTES.md; and adds a "## New in <label>" section above the
previous one: the contents of --notes, or the bullets to fill in.
"""
import argparse, pathlib, re, sys

parser = argparse.ArgumentParser()
parser.add_argument('version')
parser.add_argument('--notes', type=pathlib.Path)
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parents[2]
new = args.version
assert re.fullmatch(r'\d+\.\d+\.\d+(-rc\.\d+)?', new), new


def label(version):
    m = re.search(r'-(rc\.\d+)$', version)
    return m[1] if m else version


def anchor(version):
    return '#new-in-' + label(version).replace('.', '')


cargo = root / 'Cargo.toml'
text = cargo.read_text(encoding='utf-8')
old = re.search(r'(?m)^version = "([^"]+)"', text)[1]
assert old != new, f'already {new}'
cargo.write_text(text.replace(f'version = "{old}"', f'version = "{new}"', 1), encoding='utf-8', newline='')

# Cargo.lock: only the workspace's own crates carry the workspace version.
members = set(re.findall(r'(?m)^name = "([^"]+)"', ''.join(
    p.read_text(encoding='utf-8') for p in root.glob('rust/*/Cargo.toml'))))
lock = root / 'Cargo.lock'
text, count = re.subn(r'(?m)^(name = "(?:%s)"\nversion = )"%s"$' % ('|'.join(map(re.escape, members)), re.escape(old)),
                      lambda m: f'{m[1]}"{new}"', lock.read_text(encoding='utf-8'))
assert count, 'no workspace crate in Cargo.lock'
lock.write_text(text, encoding='utf-8', newline='')

short = re.compile(r'(?<![\w.])%s(?![\w])' % re.escape(label(old)))
# Only the short labels that name the release to download: a sentence about
# what an older release measured or changed keeps its version.
current = re.compile(r'(?<=Download ){0}(?![\w])|(?<=For ){0}(?=,)'.format(re.escape(label(old))))
for name in ('README.md', 'docs/README.md', 'docs/getting-started.md', 'rust/README.md'):
    path = root / name
    text = path.read_text(encoding='utf-8')
    for link in (f'Download {label(old)}', 'release'):
        text = text.replace(f'[{link}](https://github.com/RamazanKara/Butterpollo/releases/tag/{old})',
                            f'[{link}](https://github.com/RamazanKara/Butterpollo/releases/tag/{new})')
    text = text.replace(f'butterpollo-setup-{old}.exe', f'butterpollo-setup-{new}.exe')
    text = text.replace(f'butterpollo-rust-{old}-windows-x64.zip', f'butterpollo-rust-{new}-windows-x64.zip')
    text = text.replace(f'The workspace version is **{old}**', f'The workspace version is **{new}**')
    path.write_text(current.sub(label(new), text), encoding='utf-8', newline='')

notes = root / 'rust/RELEASE_NOTES.md'
lines = notes.read_text(encoding='utf-8').split('\n')
assert old in lines[0], lines[0]
lines[0] = lines[0].replace(old, new)
history = next(i for i, l in enumerate(lines) if l.startswith('**Release history:** '))
entry = f'[{label(new)}]({anchor(new)})'
if entry not in lines[history]:
    lines[history] = lines[history].replace('**Release history:** ',
                                            f'**Release history:** {entry} · ', 1)
intro = next(i for i, l in enumerate(lines) if i > history and l.strip())
lines[intro] = short.sub(label(new), lines[intro].replace(old, new))
if f'## New in {label(new)}' not in lines:
    at = lines.index(f'## New in {label(old)}')
    body = args.notes.read_text(encoding='utf-8').strip().split('\n') if args.notes else ['- ']
    lines[at:at] = [f'## New in {label(new)}', '', *body, '']
text = '\n'.join(lines)
text = text.replace(f'butterpollo-setup-{old}.exe` installs', f'butterpollo-setup-{new}.exe` installs')
text = text.replace(f'butterpollo-rust-{old}-windows-x64.zip`', f'butterpollo-rust-{new}-windows-x64.zip`')
notes.write_text(text, encoding='utf-8', newline='')
print(f'{old} -> {new}; {count} crates in Cargo.lock; fill in "## New in {label(new)}" in rust/RELEASE_NOTES.md'
      if not args.notes else f'{old} -> {new}; {count} crates in Cargo.lock')
