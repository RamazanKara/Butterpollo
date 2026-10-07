"""Assemble a release from the previous verified release package.

Only the rebuilt host binaries, the web console (when --web names its
build), the documentation, the lock file and the workspace versions change;
runtimes, drivers, TrueHDR and the performance probes are kept byte for byte
from the baseline, and every file is checked against the baseline's manifest
first.

usage: package.py --repo DIR --build DIR --baseline-zip ZIP --baseline-sums FILE --qa DIR --out DIR [--web DIST]
"""
import argparse
import datetime
import hashlib
import io
import json
import pathlib
import re
import shutil
import struct
import subprocess
import zipfile

parser = argparse.ArgumentParser()
for name in ('repo', 'build', 'baseline-zip', 'baseline-sums', 'qa', 'out'):
    parser.add_argument('--' + name, required=True, type=pathlib.Path)
parser.add_argument('--web', type=pathlib.Path, help='the web console built from this commit (its dist folder)')
args = parser.parse_args()
repo, build, out, qa = args.repo, args.build, args.out, args.qa
package = out / 'butterpollo-rust-release'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def sha_file(path):
    return sha(path.read_bytes())


def git(*command):
    return subprocess.check_output(['git', '-C', str(repo), *command], text=True).strip()


assert not git('status', '--porcelain'), 'source tree must be clean'
source, tree = git('rev-parse', 'HEAD'), git('rev-parse', 'HEAD^{tree}')
version = re.search(r'^version = "([^"]+)"', (repo / 'Cargo.toml').read_text(), re.M)[1]

tests = (qa / 'tests.log').read_text(encoding='utf-8-sig', errors='replace')
rows = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', tests)
passed, failed, ignored = (sum(int(r[i]) for r in rows) for i in range(3))
assert failed == 0 and passed > 0 and 'FAILED' not in tests, 'tests did not pass'
for name in ('clippy.log', 'build.log'):
    assert 'Finished' in (qa / name).read_text(encoding='utf-8-sig', errors='replace'), name

baseline_sums = {line.split()[1]: line.split()[0] for line in args.baseline_sums.read_text().splitlines()}
assert sha_file(args.baseline_zip) == baseline_sums[args.baseline_zip.name], 'baseline does not match its SHA256SUMS'
previous = re.search(r'butterpollo-rust-(.+)-windows-x64\.zip', args.baseline_zip.name)[1]
if out.exists():
    shutil.rmtree(out)
package.mkdir(parents=True)
with zipfile.ZipFile(args.baseline_zip) as z:
    base_manifest_bytes = z.read('butterpollo-rust-release/manifest.json')
    base_manifest = json.loads(base_manifest_bytes.decode('utf-8-sig'))
    for entry in base_manifest:
        data = z.read('butterpollo-rust-release/' + entry['path'])
        assert sha(data) == entry['sha256'], entry['path']
        target = package / entry['path']
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)

components = {
    'butterpollo.exe': 'butterpollo.exe',
    'butterpollo-service.exe': 'butterpollo-service.exe',
    'butterpollo-start.exe': 'Start Butterpollo.exe',
    'butterpollo_vulkan_layer.dll': 'vulkan-layer/butterpollo_vulkan_layer.dll',
}
for src, dst in components.items():
    shutil.copy2(build / src, package / dst)
sources = {
    'README.md': 'rust/README.md',
    'RELEASE_NOTES.md': 'rust/RELEASE_NOTES.md',
    'PERFORMANCE.md': 'rust/PERFORMANCE.md',
    'PARITY.md': 'rust/PARITY.md',
    'service.ps1': 'rust/service.ps1',
    'licenses/Cargo.lock': 'Cargo.lock',
    'licenses/THIRD_PARTY.md': 'rust/THIRD_PARTY.md',
    'tools/collect_environment.ps1': 'rust/tests/collect_environment.ps1',
    'tools/README.md': 'rust/tests/ENVIRONMENT_REPORT.md',
    'compatibility/moonlight-6.2.0/README.md': 'rust/compatibility/moonlight-6.2.0/README.md',
    'compatibility/moonlight-6.2.0/moonlight-v6.2.0-cli-cached-artwork.patch':
        'rust/compatibility/moonlight-6.2.0/moonlight-v6.2.0-cli-cached-artwork.patch',
}
for dst, src in sources.items():
    shutil.copy2(repo / src, package / dst)
# The web console from this commit. Its file names carry content hashes, so
# they change with it; everything else keeps the baseline's layout.
WEB = 'assets/web/'
if args.web:
    assert (args.web / 'index.html').is_file(), f'{args.web} is not a web console build'
    shutil.rmtree(package / WEB)
    shutil.copytree(args.web, package / WEB)
deps_path = package / 'licenses/rust-dependencies.json'
deps = json.loads(deps_path.read_text(encoding='utf-8-sig'))
workspace = {'butterpollo', 'butterpollo-core', 'butterpollo-windows', 'butterpollo-setup',
             'butterpollo-vulkan-layer', 'butterpollo-truehdr-runtime'}
for dep in deps:
    if dep['name'] in workspace:
        dep['version'] = version
deps_path.write_text(json.dumps(deps, indent=2), encoding='utf-8')

entries = [dict(path=p.relative_to(package).as_posix(), sha256=sha_file(p))
           for p in sorted(package.rglob('*'))
           if p.is_file() and p.relative_to(package).as_posix() != 'manifest.json']
def layout(paths):
    return {p for p in paths if not (args.web and p.startswith(WEB))}
assert layout(e['path'] for e in entries) == layout(e['path'] for e in base_manifest), 'package layout changed'
(package / 'manifest.json').write_text(json.dumps(entries, indent=2), encoding='utf-8')
base = {e['path']: e['sha256'] for e in base_manifest}
changed = sorted(e['path'] for e in entries if base.get(e['path']) != e['sha256'])

archive = out / f'butterpollo-rust-{version}-windows-x64.zip'
with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as z:
    for p in sorted(package.rglob('*')):
        if p.is_file():
            z.write(p, p.relative_to(out).as_posix())
payload = archive.read_bytes()
stub = (build / 'butterpollo-setup.exe').read_bytes()
installer = out / f'butterpollo-setup-{version}.exe'
installer.write_bytes(stub + payload + b'BPSETUP1' + struct.pack('<Q', len(payload)))
# Read the installer back exactly as setup does.
assembled = installer.read_bytes()
assert assembled[-16:-8] == b'BPSETUP1'
size = struct.unpack('<Q', assembled[-8:])[0]
assert assembled[:-16 - size] == stub and assembled[-16 - size:-16] == payload
with zipfile.ZipFile(io.BytesIO(assembled[-16 - size:-16])) as z:
    assert z.testzip() is None
    for entry in entries:
        assert sha(z.read('butterpollo-rust-release/' + entry['path'])) == entry['sha256']

tracked = git('ls-files', 'Cargo.toml', 'Cargo.lock', 'rust').splitlines()
(out / 'SOURCE-MANIFEST.json').write_text(
    json.dumps([dict(path=p, sha256=sha_file(repo / p)) for p in tracked], indent=2) + '\n', encoding='utf-8')
components_sha = {src: sha_file(build / src) for src in [*components, 'butterpollo-setup.exe']}
assets = [dict(name=p.name, bytes=p.stat().st_size, sha256=sha_file(p)) for p in (installer, archive)]
common = dict(version=version, source_commit=source, source_tree=tree, branch='main',
              host_sha256=components_sha['butterpollo.exe'], build_component_sha256=components_sha,
              assets=assets, source_manifest_sha256=sha_file(out / 'SOURCE-MANIFEST.json'))
provenance = dict(common, installed=False,
                  baseline=dict(version=previous, manifest_sha256=sha(base_manifest_bytes),
                                zip_sha256=baseline_sums[args.baseline_zip.name],
                                retained_files=len(entries) - len(changed)),
                  build_origin='Local Windows Rust GNU release build of the exact source commit.')
validation = dict(common, installed=False,
                  packaged_at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  manifest_entries=len(entries), changed_manifest_entries=changed,
                  checks=dict(ordinary_passed=passed, failed=failed, ignored=ignored,
                              clippy='passed with warnings denied', formatting='passed',
                              release_build='passed',
                              package='All manifest hashes, ZIP CRCs and the embedded payload, stub and footer verified.'))
(out / 'BUILD_PROVENANCE.json').write_text(json.dumps(provenance, indent=2) + '\n', encoding='utf-8')
(out / 'VALIDATION.json').write_text(json.dumps(validation, indent=2) + '\n', encoding='utf-8')
print(json.dumps(dict(version=version, source_commit=source, passed=passed, changed=changed, assets=assets), indent=2))
