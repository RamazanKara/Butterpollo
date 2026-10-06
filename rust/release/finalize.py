"""Record the end-to-end, protocol, self-test, install and CI results in a
packaged release and write its SHA256SUMS.

usage: finalize.py --out DIR --work DIR --changes FILE [--ci FILE] [--scope TEXT] [--installed]
"""
import argparse, hashlib, json, pathlib

parser = argparse.ArgumentParser()
parser.add_argument('--out', required=True, type=pathlib.Path)
parser.add_argument('--work', required=True, type=pathlib.Path)
parser.add_argument('--changes', required=True, type=pathlib.Path)
parser.add_argument('--ci', type=pathlib.Path, help='gh run list --json status,conclusion,url,headSha output')
parser.add_argument('--scope', default='')
parser.add_argument('--installed', action='store_true')
args = parser.parse_args()
out, work = args.out, args.work


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


validation = json.loads((out / 'VALIDATION.json').read_text(encoding='utf-8'))
provenance = json.loads((out / 'BUILD_PROVENANCE.json').read_text(encoding='utf-8'))
version = validation['version']
for asset in validation['assets']:
    assert sha(out / asset['name']) == asset['sha256'], asset['name']

results = sorted(work.glob('e2e-*/result.json'))
streams = [json.loads(p.read_text()) for p in results if not p.parent.name.endswith('-failed')]
assert streams and all(s['passed'] for s in streams), 'an end-to-end stream failed'
# A stream that failed once and passed when run again is recorded with both runs.
for failed in (json.loads(p.read_text()) for p in results if p.parent.name.endswith('-failed')):
    next(s for s in streams if s['codec'] == failed['codec'])['first_attempt_failed'] = failed
protocol = json.loads((work / 'protocol' / 'result.json').read_text())
assert protocol['passed'], 'a protocol check failed'
validation['changes'] = [line for line in args.changes.read_text(encoding='utf-8').splitlines() if line.strip()]
validation['end_to_end'] = dict(
    scope='Packaged host with an isolated profile; the independent moonlight-common-c client pairs over loopback '
          'with a PIN, launches over encrypted RTSP and decodes video and audio. WGC capture of the physical '
          'desktop; no virtual display, display-mode or HDR change.',
    streams=streams, protocol_checks=protocol['checks'])

ci = json.loads(args.ci.read_text(encoding='utf-8-sig') or 'null') if args.ci else None
if ci:
    # Published without waiting for CI; it verifies the same commit afterwards.
    assert ci['headSha'] == validation['source_commit'], (ci['headSha'], validation['source_commit'])
    assert ci['conclusion'] in ('success', '', None), ci
for document in (validation, provenance):
    document['independent_ci'] = ci
    document['installed'] = args.installed

if args.installed:
    installed = json.loads((work / 'elevated' / 'installed.json').read_text(encoding='utf-8-sig'))
    assert installed['setup_exit'] == 0 and installed['reported_version'] == version, installed
    assert installed['host_sha256'] == validation['host_sha256'], 'the installed host is not this build'
    validation['installation'] = installed
    self_test = work / 'elevated' / 'display-self-test.json'
    if self_test.exists():
        validation['display_self_test'] = json.loads(self_test.read_text(encoding='utf-8-sig'))
if args.scope:
    validation['hardware_scope'] = args.scope

(out / 'VALIDATION.json').write_text(json.dumps(validation, indent=2) + '\n', encoding='utf-8')
(out / 'BUILD_PROVENANCE.json').write_text(json.dumps(provenance, indent=2) + '\n', encoding='utf-8')
names = [a['name'] for a in validation['assets']] + ['BUILD_PROVENANCE.json', 'VALIDATION.json', 'SOURCE-MANIFEST.json']
(out / 'SHA256SUMS').write_text(''.join(f'{sha(out / n)}  {n}\n' for n in names), encoding='ascii')
print((out / 'SHA256SUMS').read_text(), end='')
