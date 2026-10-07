"""Stream a packaged host end to end, isolated from the installed one.

Starts the package's host with its own profile and ports (48518-48544),
pairs the independent moonlight-common-c client through rust/tests/interop.py,
launches the Desktop app, streams and decodes video and audio, then unpairs
and stops. A narrow moving strip and a virtual-speaker tone exercise capture.
No display-mode, HDR or default-audio change; the installed host must be idle.

usage: e2e.py --package DIR --work DIR --client EXE --codec CODEC [--mode 1280x720x60] [--seconds 12]
Writes WORK/e2e-CODEC/result.json.
"""
import argparse, json, pathlib, subprocess, sys, time
import xml.etree.ElementTree as ET
import requests
from e2e_result import evaluate
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
args = parser.parse_args()
interop = pathlib.Path(__file__).resolve().parents[1] / 'tests' / 'interop.py'

installed_idle()
plain = requests.Session(); plain.trust_env = False

case = args.work / (f'e2e-{args.codec}' + ('-vrr' if args.vrr else ''))
assert case.resolve().parent == args.work.resolve(), 'test output must stay inside the work directory'
if case.exists():
    import shutil; shutil.rmtree(case)
env, display, sink = prepare(args, case)
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
    motion = spawn([str(motion_probe), display['display_name'], fixture_seconds, str(case / 'motion.json'), str(int(fps) * 2), '128'], 'motion.log', env=env)
    time.sleep(.5)
    assert tone.poll() is None and motion.poll() is None, 'audio or motion fixture exited'
    client_env = receiver_environment(env, display, args.mode, args.client, args.vrr)
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
