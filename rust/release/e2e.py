"""Stream a packaged host end to end, isolated from the installed one.

Starts the package's host with its own profile and ports (48518-48544),
pairs the independent moonlight-common-c client through rust/tests/interop.py,
launches the Desktop app, streams and decodes video and audio, then unpairs
and stops. No display, HDR or audio change; the installed host must be idle.

usage: e2e.py --package DIR --work DIR --client EXE --codec CODEC [--mode 1280x720x60] [--seconds 12]
Writes WORK/e2e-CODEC/result.json.
"""
import argparse, hashlib, json, os, pathlib, re, subprocess, sys, time
import xml.etree.ElementTree as ET
import requests

parser = argparse.ArgumentParser()
parser.add_argument('--package', required=True, type=pathlib.Path)
parser.add_argument('--work', required=True, type=pathlib.Path)
parser.add_argument('--client', required=True, type=pathlib.Path)
parser.add_argument('--codec', required=True)
parser.add_argument('--mode', default='1280x720x60')
parser.add_argument('--seconds', default='12')
parser.add_argument('--bitrate', default='20000')
args = parser.parse_args()
width, height, fps = args.mode.split('x')
interop = pathlib.Path(__file__).resolve().parents[1] / 'tests' / 'interop.py'

plain = requests.Session(); plain.trust_env = False
info = ET.fromstring(plain.get('http://127.0.0.1:47989/serverinfo', timeout=3).text)
assert info.findtext('state') == 'SUNSHINE_SERVER_FREE', 'the installed host is streaming'
assert info.findtext('RustHostApplicationActive') in (None, '0'), 'the installed host runs an app'

case = args.work / f'e2e-{args.codec}'
if case.exists():
    import shutil; shutil.rmtree(case)
profile = case / 'config'; (profile / 'logs').mkdir(parents=True)
env = os.environ.copy(); env.update(PATH=str(args.package) + ';' + env['PATH'], RUST_LOG='info', NO_PROXY='*')
host_exe = args.package / 'butterpollo.exe'
diag = json.loads(subprocess.check_output([str(host_exe), '--diagnostics'], env=env, text=True))
monitor = next(m for m in diag['monitors'] if m['primary'])
(profile / 'sunshine.conf').write_text('\n'.join([
    'port = 48523', 'encoder = amf', 'capture = wgc', f"output_name = {monitor['device_id']}",
    f'minimum_fps_target = {fps}', 'virtual_display_mode = disabled', 'dd_configuration_option = disabled',
    'dd_resolution_option = disabled', 'dd_refresh_rate_option = disabled', 'dd_hdr_option = disabled',
    'dd_always_restore_from_golden = false', 'dd_config_revert_on_disconnect = false',
    'frame_limiter_enable = false', 'install_steam_audio_drivers = false', 'stream_audio = false',
    'keep_sink_default = false', 'upnp = false', 'enable_discovery = false', 'vulkan_hdr_layer = false',
    'system_tray = false', 'update_check_interval = 0', 'pyrowave = false', '']))
(profile / 'apps.json').write_text(json.dumps({'apps': [{'name': 'Desktop', 'cmd': '', 'virtual-display': False}]}))
# interop.py's fixture account, in the previous host's format.
salt = os.urandom(8).hex()
(profile / 'sunshine_state.json').write_text(json.dumps({'username': 'test', 'salt': salt, 'password': hashlib.sha256(('rust-smoke-only' + salt).encode()).digest()[::-1].hex().upper()}))

owned = []
def spawn(command, name, **kw):
    f = (case / name).open('w')
    p = subprocess.Popen(command, stdout=f, stderr=subprocess.STDOUT, creationflags=subprocess.CREATE_NO_WINDOW, **kw)
    owned.append((p, f)); return p
try:
    host = spawn([str(host_exe), '--config-dir', str(profile), '--assets', str(args.package / 'assets' / 'web'), '--bind', '127.0.0.1', '--no-tray'], 'host.stdout.log', cwd=case, env=env)
    log = profile / 'logs' / 'butterpollo.log'
    deadline = time.monotonic() + 60
    while True:
        assert host.poll() is None, 'host exited'
        if log.exists() and 'encoder capability probe completed' in log.read_text(errors='replace'):
            try:
                if plain.get('http://127.0.0.1:48523/serverinfo', timeout=1).ok: break
            except requests.RequestException: pass
        assert time.monotonic() < deadline, 'host not ready'
        time.sleep(.2)
    client_env = env.copy()
    client_env.update(BUTTERPOLLO_TEST_PORT='48523', BUTTERPOLLO_TEST_CLIENT_EXE=str(args.client), BUTTERPOLLO_TEST_MATCH_DISPLAY='1',
                      BUTTERPOLLO_TEST_REQUIRE_PICTURE='1', BUTTERPOLLO_TEST_WARMUP_SECONDS='3',
                      BUTTERPOLLO_TEST_MIN_FPS=str(int(float(fps) * .9)))
    (case / 'receiver').mkdir()
    receiver = spawn([sys.executable, str(interop), str(case / 'receiver'), args.codec, width, height, fps, args.seconds, args.bitrate, '4'], 'client.log', env=client_env)
    rc = receiver.wait(timeout=int(args.seconds) + 120)
finally:
    for p, f in reversed(owned):
        if p.poll() is None: p.terminate()
        try: p.wait(timeout=10)
        except subprocess.TimeoutExpired: p.kill(); p.wait()
        f.close()

client = (case / 'client.log').read_text(errors='replace')
def find(pattern, cast=float):
    m = re.search(pattern, client); return cast(m.group(1)) if m else None
result = dict(codec=args.codec, mode=args.mode, client_exit=rc, passed=rc == 0 and 'INTEROPERABILITY PASS' in client,
              frames=find(r'RESULT frames=(\d+)', int), decoded=find(r'decoded_frames=(\d+)', int),
              audio_packets=find(r'audio_packets=(\d+)', int), decode_failures=find(r'failures=(\d+)', int),
              steady_fps=find(r'STEADY warmup_seconds=\S+ seconds=\S+ frames=\d+ fps=([0-9.]+)'),
              long_intervals=find(r'intervals_over_1_5_period=(\d+)', int),
              host_mean_ms=find(r'STEADY_HOST .*?mean_ms=([0-9.]+)'), host_p99_ms=find(r'STEADY_HOST .*?p99_ms=([0-9.]+)'))
(case / 'result.json').write_text(json.dumps(result, indent=2))
print(json.dumps(result))
sys.exit(0 if result['passed'] else 1)
