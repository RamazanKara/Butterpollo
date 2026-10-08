"""Write VALIDATION.json for a published release from check.ps1's results:
the end-to-end streams, protocol checks, display self-test and install.

usage: validation.py --version V --package DIR --work DIR --out FILE [--scope TEXT] [--seconds N]
                     [--real-client FILE] [--installed] [--skipped CASE ...]
"""
import argparse, datetime, hashlib, json, pathlib

parser = argparse.ArgumentParser()
parser.add_argument('--version', required=True)
parser.add_argument('--package', required=True, type=pathlib.Path)
parser.add_argument('--work', required=True, type=pathlib.Path)
parser.add_argument('--out', required=True, type=pathlib.Path)
parser.add_argument('--scope', default='')
parser.add_argument('--seconds', type=int, help='how long the host check took')
parser.add_argument('--real-client', type=pathlib.Path, help="the laptop's smoke.ps1 result")
parser.add_argument('--installed', action='store_true')
# Stream cases left out on purpose (check.ps1 -SkipStreams); recorded, never silent.
parser.add_argument('--skipped', action='append', default=[])
args = parser.parse_args()
work, version = args.work, args.version


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


results = sorted(work.glob('e2e-*/result.json'))
streams = [json.loads(p.read_text()) for p in results if not p.parent.name.endswith('-failed')]
assert streams and all(s['passed'] for s in streams), 'an end-to-end stream failed'
skipped = set(args.skipped)
required = {('h264', False), ('hevc', False), ('av1', False), ('hevc', True), ('pyrowave', False), ('pyrowave-hdr-444', False)}
required -= {(case.removesuffix('-vrr'), case.endswith('-vrr')) for case in skipped}
assert required <= {(s['codec'], s.get('vrr', False)) for s in streams}, 'the fixed-rate, VRR or PyroWave release matrix is incomplete'
assert ({'pyrowave', 'pyrowave-hdr-444'} - skipped) <= {s['codec'] for s in streams if s.get('mode') == '1920x1080x60' and not s.get('vrr', False)}, '1080p60 SDR and HDR PyroWave results are required'
assert all(s.get('audio_continuous') == 1 and s.get('motion_coverage', 0) >= .95 for s in streams), 'real audio and motion measurements are required'
# A stream that failed once and passed when run again is recorded with both runs.
for failed in (json.loads(p.read_text()) for p in results if p.parent.name.endswith('-failed')):
    next(s for s in streams if (s['codec'], s['vrr']) == (failed['codec'], failed['vrr']))['first_attempt_failed'] = failed
protocol = json.loads((work / 'protocol' / 'result.json').read_text())
assert protocol['passed'], 'a protocol check failed'

host_sha256 = sha(args.package / 'butterpollo.exe')
validation = dict(
    version=version, host_sha256=host_sha256,
    checked_at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
    end_to_end=dict(
        scope='Packaged host with an isolated profile; the independent moonlight-common-c client pairs over loopback '
              'with a PIN, launches over encrypted RTSP and decodes video and audio. WGC capture of the physical '
              'desktop; no virtual display, display-mode or HDR change.',
        streams=streams, protocol_checks=protocol['checks'], skipped_streams=sorted(skipped)),
    installed=args.installed, check_seconds=args.seconds,
    real_client=json.loads(args.real_client.read_text(encoding='utf-8-sig')) if args.real_client else 'skipped')
if args.installed:
    installed = json.loads((work / 'elevated' / 'installed.json').read_text(encoding='utf-8-sig'))
    assert installed['setup_exit'] == 0 and installed['reported_version'] == version, installed
    assert installed['host_sha256'] == host_sha256, 'the installed host is not this release'
    validation['installation'] = installed
    self_test = work / 'elevated' / 'display-self-test.json'
    if self_test.exists():
        validation['display_self_test'] = json.loads(self_test.read_text(encoding='utf-8-sig'))
if args.scope:
    validation['hardware_scope'] = args.scope
args.out.write_text(json.dumps(validation, indent=2) + '\n', encoding='utf-8')
real = validation['real_client']
print(f'{version}: {len(streams)} streams, protocol checks and {"install" if args.installed else "no install"} passed; '
      f'real-client smoke test {"skipped" if real == "skipped" else "passed" if real["passed"] else "FAILED"}')
