"""Stream a packaged host end to end, isolated from the installed one.

Starts the package's host with its own profile and ports (48518-48544),
pairs the independent moonlight-common-c client through rust/tests/interop.py,
launches the Desktop app, streams and decodes video and audio, then unpairs
and stops. A narrow moving strip and a virtual-speaker tone exercise capture.
No display-mode, HDR or default-audio change; the installed host must be idle.

usage: e2e.py --package DIR --work DIR --client EXE --codec CODEC [--mode 1280x720x60] [--seconds 12]
              [--vrr] [--recovery N] [--motion-at-rate]
Writes WORK/e2e-CODEC[-vrr][-recovery|-at-rate]/result.json.

--recovery N: the receiver asks for a keyframe N times, every
RECOVERY_INTERVAL_MS, as a client losing packets does, while the moving
strip runs at the stream rate (a game held there by a frame limit) and the
minimum frame rate keeps its default of 20. Each request must ride the
next new frame instead of the unchanged picture being encoded again.

The host traces every claim (RUST_LOG=info,pacing=trace) and the stream
fails if it encoded an unchanged picture again; the receiver's count of
pictures seen twice or skipped is reported only, since the strip at the
stream rate repeats and skips some pictures by itself.

--motion-at-rate: the same strip, minimum frame rate and trace without
requests, the control for --recovery. BUTTERPOLLO_E2E_RUST_LOG replaces the
host's RUST_LOG.
"""
import argparse, json, pathlib, subprocess, sys, time
import xml.etree.ElementTree as ET
import requests
from e2e_result import evaluate, host_frames
from e2e_host import installed_idle, prepare, receiver_environment

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
parser.add_argument('--recovery', type=int, default=0, metavar='N', help='keyframe requests during the stream, with the motion strip at the stream rate')
parser.add_argument('--motion-at-rate', action='store_true', help='the strip at the stream rate without requests: the control for --recovery')
args = parser.parse_args()
RECOVERY_INTERVAL_MS = 500
at_rate = bool(args.recovery) or args.motion_at_rate
if at_rate:
    # The default an unconfigured host has; the release streams resend at the
    # full rate, which would add repeats of their own.
    args.config.insert(0, 'minimum_fps_target=20')
interop = pathlib.Path(__file__).resolve().parents[1] / 'tests' / 'interop.py'

installed_idle()
plain = requests.Session(); plain.trust_env = False

case = args.work / (f'e2e-{args.codec}' + ('-vrr' if args.vrr else '')
                    + ('-recovery' if args.recovery else '-at-rate' if args.motion_at_rate else ''))
assert case.resolve().parent == args.work.resolve(), 'test output must stay inside the work directory'
if case.exists():
    import shutil; shutil.rmtree(case)
# The keyframe-request stream is judged on the host's per-claim trace.
env, display, sink = prepare(args, case, rust_log='info,pacing=trace' if at_rate else 'info')
profile = case / 'config'
host_exe = args.package / 'butterpollo.exe'
audio_probe = args.client.parent / 'audio_probe.exe'
motion_probe = args.client.parent / 'motion_probe.exe'
width, height, fps = args.mode.split('x')

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
    fixture_seconds = str(int(args.seconds) + 90)
    tone = spawn([str(audio_probe), fixture_seconds, sink['id'], '--render-only'], 'tone.log', env=env)
    motion_rate = int(fps) if at_rate else int(fps) * 2
    motion = spawn([str(motion_probe), display['display_name'], fixture_seconds, str(case / 'motion.json'), str(motion_rate), '128'], 'motion.log', env=env)
    time.sleep(.5)
    assert tone.poll() is None and motion.poll() is None, 'audio or motion fixture exited'
    client_env = receiver_environment(env, display, args.mode, args.client, args.vrr)
    if args.recovery:
        client_env.update(BUTTERPOLLO_TEST_IDR_PROBE=str(args.recovery),
                          BUTTERPOLLO_TEST_IDR_PROBE_INTERVAL_MS=str(RECOVERY_INTERVAL_MS))
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
config = dict(line.split('=', 1) for line in (profile / 'sunshine.conf').read_text().splitlines() if '=' in line)
config = {key.strip(): value.strip() for key, value in config.items()}
requested_source = dict(width=display['width'], height=display['height'], refresh_hz=display['refresh_hz'],
                        pixel='RgbaF16' if display['hdr'] else 'Bgra8')
result = evaluate(client, rc, args.codec, args.mode, args.vrr, recovery=args.recovery,
                  requested_capture=config['capture'], requested_source=requested_source,
                  host_log=log.read_text(errors='replace') if log.exists() else '',
                  tone_log=(case / 'tone.log').read_text(errors='replace'),
                  host_frames=host_frames(case / 'receiver'))
(case / 'result.json').write_text(json.dumps(result, indent=2))
print(json.dumps(result))
sys.exit(0 if result['passed'] else 1)
