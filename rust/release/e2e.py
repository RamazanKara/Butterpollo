"""Stream a packaged host end to end, isolated from the installed one.

Starts the package's host with its own profile and ports (48518-48544),
pairs the independent moonlight-common-c client through rust/tests/interop.py,
launches the Desktop app, streams and decodes video and audio, then unpairs
and stops. A narrow moving strip and a virtual-speaker tone exercise capture.
No display-mode, HDR or default-audio change; the installed host must be idle.

usage: e2e.py --package DIR --work DIR --client EXE --codec CODEC [--mode 1280x720x60] [--seconds 12]
Writes WORK/e2e-CODEC/result.json.
"""
import argparse, hashlib, json, os, pathlib, subprocess, sys, time
import xml.etree.ElementTree as ET
import requests
from e2e_result import evaluate

parser = argparse.ArgumentParser()
parser.add_argument('--package', required=True, type=pathlib.Path)
parser.add_argument('--work', required=True, type=pathlib.Path)
parser.add_argument('--client', required=True, type=pathlib.Path)
parser.add_argument('--codec', required=True)
parser.add_argument('--mode', help='defaults to 720 pixels high at 60 FPS, matching the desktop aspect ratio')
parser.add_argument('--seconds', default='12')
parser.add_argument('--bitrate', default='20000')
parser.add_argument('--vrr', action='store_true', help='launch as a client asking for VRR')
parser.add_argument('--config', action='append', default=[], metavar='KEY=VALUE', help='an extra sunshine.conf line for the host, e.g. wgc_user_helper=true')
args = parser.parse_args()
interop = pathlib.Path(__file__).resolve().parents[1] / 'tests' / 'interop.py'

installed_log = pathlib.Path(r'C:\ProgramData\Butterpollo\config\logs\butterpollo.log')
events = [line for line in installed_log.read_text(errors='replace').splitlines()
          if 'CLIENT CONNECTED' in line or 'CLIENT DISCONNECTED' in line]
assert not events or 'CLIENT DISCONNECTED' in events[-1], 'the installed host log has an active stream'
plain = requests.Session(); plain.trust_env = False
info = ET.fromstring(plain.get('http://127.0.0.1:47989/serverinfo', timeout=3).text)
assert info.findtext('state') == 'SUNSHINE_SERVER_FREE', 'the installed host is streaming'
assert info.findtext('RustHostSessionCount') in (None, '0'), 'the installed host has a session'
assert info.findtext('RustHostPendingSessionCount') in (None, '0'), 'the installed host has a pending session'
assert info.findtext('RustHostApplicationActive') in (None, '0'), 'the installed host runs an app'

case = args.work / (f'e2e-{args.codec}' + ('-vrr' if args.vrr else ''))
assert case.resolve().parent == args.work.resolve(), 'test output must stay inside the work directory'
if case.exists():
    import shutil; shutil.rmtree(case)
profile = case / 'config'; (profile / 'logs').mkdir(parents=True)
env = os.environ.copy(); env.update(PATH=str(args.package) + ';' + env['PATH'], RUST_LOG='info', NO_PROXY='*')
host_exe = args.package / 'butterpollo.exe'
diag = json.loads(subprocess.check_output([str(host_exe), '--diagnostics'], env=env, text=True))
monitor = next(m for m in diag['monitors'] if m['primary'])
display = next(d for d in diag['displays'] if d['display_name'] == monitor['display_name'])
if args.mode is None:
    args.mode = f"{round(display['width'] * 720 / display['height'] / 2) * 2}x720x60"
width, height, fps = args.mode.split('x')
scale = min(int(width) / display['width'], int(height) / display['height'])
# Match the encoder's centred letterbox without changing the desktop mode.
content_width = min(int(width), max(2, int(display['width'] * scale + .5) & ~1))
content_height = min(int(height), max(2, int(display['height'] * scale + .5) & ~1))
left = ((int(width) - content_width) // 2) & ~1
bottom = int(height) - content_height - (((int(height) - content_height) // 2) & ~1)
audio_probe = args.client.parent / 'audio_probe.exe'
motion_probe = args.client.parent / 'motion_probe.exe'
assert audio_probe.is_file() and motion_probe.is_file(), 'build the audio and motion probes beside the receiver'
endpoints = json.loads(subprocess.check_output([str(audio_probe), '--list'], env=env, text=True))
sink = next((e for e in endpoints if e['virtual_sink']), None)
assert sink, 'Steam Streaming Speakers is required for the isolated audio test'
defaults = [
    'port = 48523', 'encoder = amf', 'capture = wgc', f"output_name = {monitor['device_id']}",
    f'minimum_fps_target = {fps}', 'virtual_display_mode = disabled', 'dd_configuration_option = disabled',
    'dd_resolution_option = disabled', 'dd_refresh_rate_option = disabled', 'dd_hdr_option = disabled',
    'dd_always_restore_from_golden = false', 'dd_config_revert_on_disconnect = false',
    'frame_limiter_enable = false', 'install_steam_audio_drivers = false', 'stream_audio = true',
    'audio_sink_capture_only = true', 'auto_capture_sink = false', f"audio_sink = {sink['id']}",
    'keep_sink_default = false', 'upnp = false', 'enable_discovery = false', 'vulkan_hdr_layer = false',
    'system_tray = false', 'update_check_interval = 0', f'pyrowave = {str(args.codec.startswith("pyrowave")).lower()}']
if args.codec.startswith('pyrowave'):
    defaults.append('lan_encryption_mode = 2')
# A --config line replaces the default line of the same key.
key = lambda line: line.split('=', 1)[0].strip()
overrides = [f'{key(line)} = {line.split("=", 1)[1].strip()}' for line in args.config]
overridden = {key(line) for line in args.config}
(profile / 'sunshine.conf').write_text('\n'.join(
    [line for line in defaults if key(line) not in overridden] + overrides + ['']))
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
    fixture_seconds = str(min(300, int(args.seconds) + 90))
    tone = spawn([str(audio_probe), fixture_seconds, sink['id'], '--render-only'], 'tone.log', env=env)
    motion = spawn([str(motion_probe), display['display_name'], fixture_seconds, str(case / 'motion.json'), str(int(fps) * 2), '128'], 'motion.log', env=env)
    time.sleep(.5)
    assert tone.poll() is None and motion.poll() is None, 'audio or motion fixture exited'
    client_env = env.copy()
    client_env.update(BUTTERPOLLO_TEST_HOST='127.0.0.1', BUTTERPOLLO_TEST_PORT='48523',
                      BUTTERPOLLO_TEST_CLIENT_EXE=str(args.client), BUTTERPOLLO_TEST_MATCH_DISPLAY='1',
                      BUTTERPOLLO_TEST_REQUIRE_PICTURE='1', BUTTERPOLLO_TEST_WARMUP_SECONDS='3',
                      BUTTERPOLLO_TEST_MIN_FPS=str(float(fps) * .97), BUTTERPOLLO_TEST_AUDIO_TONE='1',
                      BUTTERPOLLO_TEST_REQUIRE_MOTION='1', BUTTERPOLLO_TEST_BARCODE_BOTTOM='1',
                      BUTTERPOLLO_TEST_BARCODE_LEFT=str(left), BUTTERPOLLO_TEST_BARCODE_BOTTOM_MARGIN=str(bottom),
                      BUTTERPOLLO_TEST_BARCODE_SCALE=str(scale), BUTTERPOLLO_TEST_VRR='1' if args.vrr else '0')
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
result = evaluate(client, rc, args.codec, args.mode, args.vrr,
                  tone_log=(case / 'tone.log').read_text(errors='replace'))
(case / 'result.json').write_text(json.dumps(result, indent=2))
print(json.dumps(result))
sys.exit(0 if result['passed'] else 1)
