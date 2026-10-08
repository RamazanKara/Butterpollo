"""Repeatable, bounded soak against one test-owned host. See soak.ps1."""
import argparse, concurrent.futures, ctypes, hashlib, json, os, pathlib, re
import socket, subprocess, sys, time
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from types import SimpleNamespace
import requests, urllib3
from e2e_host import INSTALLED_LOG, installed_idle, prepare, receiver_environment
from e2e_result import evaluate
from soak_result import audio_windows, fault_outcome, resource_growth, video_windows

ROOT = pathlib.Path(__file__).resolve().parent
PORT = 48723
urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)


def write(path, value):
    path.write_text(json.dumps(value, indent=2), encoding='utf-8')


def wait_idle(host_pid=None, overlap_log=None):
    while True:
        try:
            installed_idle()
            state = snapshot(os.getpid(), processes_only=True)
            overlap = [p for p in state['processes'] if p['ProcessId'] != host_pid and p['ParentProcessId'] != host_pid
                       and ((p['Name'] == 'butterpollo.exe' and '--config-dir' in (p['CommandLine'] or '')
                             # The installed host and its display watcher (rc.23+).
                             and '--service-stop-source' not in p['CommandLine']
                             and '--display-watch' not in p['CommandLine'])
                            or re.search(r'(gpu_load|_probe)\.exe$', p['Name']))]
            if overlap:
                if overlap_log:
                    with overlap_log.open('a') as output:
                        output.write(json.dumps(dict(utc=state['utc'], processes=overlap)) + '\n')
                raise AssertionError('another isolated stream test is running')
            return
        except (AssertionError, requests.RequestException) as error:
            print(f'PAUSED: installed host is not confirmed idle: {error}', flush=True)
            time.sleep(5)


def snapshot(pid, processes_only=False):
    command = ['pwsh', '-NoProfile', '-File', str(ROOT / 'soak_sample.ps1'), '-HostId', str(pid)]
    if processes_only:
        command.append('-ProcessesOnly')
    result = subprocess.run(command,
                            capture_output=True, text=True, timeout=20, creationflags=subprocess.CREATE_NO_WINDOW)
    if result.returncode:
        raise RuntimeError(result.stderr)
    state = json.loads(result.stdout)
    if not processes_only:
        xinput = ctypes.WinDLL('xinput1_4').XInputGetState
        state['xinput_slots'] = [slot for slot in range(4) if xinput(slot, ctypes.create_string_buffer(32)) == 0]
    return state


def create_job():
    # Closing this handle also stops capture helpers and receivers if the host
    # or pairing script crashes before it can clean up its own children.
    from ctypes import wintypes as w
    class Basic(ctypes.Structure):
        _fields_ = [('process_time', ctypes.c_int64), ('job_time', ctypes.c_int64), ('flags', w.DWORD),
                    ('minimum', ctypes.c_size_t), ('maximum', ctypes.c_size_t), ('active', w.DWORD),
                    ('affinity', ctypes.c_size_t), ('priority', w.DWORD), ('scheduling', w.DWORD)]
    class Extended(ctypes.Structure):
        _fields_ = [('basic', Basic), ('io', ctypes.c_uint64 * 6),
                    ('process_memory', ctypes.c_size_t), ('job_memory', ctypes.c_size_t),
                    ('peak_process', ctypes.c_size_t), ('peak_job', ctypes.c_size_t)]
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.CreateJobObjectW.restype = w.HANDLE
    kernel.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
    kernel.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
    kernel.CloseHandle.argtypes = [w.HANDLE]
    handle = kernel.CreateJobObjectW(None, None)
    if not handle:
        raise ctypes.WinError(ctypes.get_last_error())
    info = Extended(); info.basic.flags = 0x2000
    if not kernel.SetInformationJobObject(handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
        kernel.CloseHandle(handle)
        raise ctypes.WinError(ctypes.get_last_error())
    return kernel, handle


def cases(mode):
    duration = 1200 if mode == 'long' else 25
    rows = []
    for index, (codec, fps, vrr) in enumerate([('hevc', 60, False), ('av1', 60, False), ('hevc', 120, True)]):
        for load in ([False, True] if index % 2 == 0 else [True, False]):
            rows.append(dict(name=f'{codec}-{fps}-' + ('load' if load else 'idle'), codec=codec,
                             mode=f'1920x1080x{fps}', seconds=duration, vrr=vrr, load=load))
    for index, (codec, size) in enumerate([('h264', '1920x1080'), ('hevc', '1920x1080'),
            ('av1', '1920x1080'), ('pyrowave', '1920x1080'), ('hevc-hdr', '1920x1080'),
            ('hevc', '1280x720'), ('pyrowave-hdr-444', '1920x1080'), ('h264', '1920x1080')]):
        rows.append(dict(name=f'switch-{index + 1}-{codec}', codec=codec, mode=size + 'x60', seconds=10))
    for index in range(52):
        rows.append(dict(name=f'churn-{index + 1:02}', codec='hevc', mode='1920x1080x60', seconds=3,
                         churn=True, resume=index % 2 == 1))
    for fault in ('wgc-helper', 'DXGI_ERROR_ACCESS_LOST', 'encoder failure', 'encoder failure.persistent'):
        rows.append(dict(name='fault-' + fault.replace(' ', '-'), codec='hevc', mode='1920x1080x60', seconds=20, fault=fault))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=['short', 'long'], default='short')
    parser.add_argument('--package', required=True, type=pathlib.Path)
    parser.add_argument('--client', required=True, type=pathlib.Path)
    parser.add_argument('--gpu-load', required=True, type=pathlib.Path)
    parser.add_argument('--work', required=True, type=pathlib.Path)
    args = parser.parse_args()
    for name in ('package', 'client', 'gpu_load', 'work'):
        setattr(args, name, getattr(args, name).resolve())
    for path in (args.package / 'butterpollo.exe', args.client, args.gpu_load,
                 args.client.parent / 'audio_probe.exe', args.client.parent / 'motion_probe.exe'):
        if not path.is_file():
            parser.error(f'missing fixture: {path}')
    args.work.mkdir(parents=True, exist_ok=True)
    run = args.work / datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S-%fZ')
    run.mkdir()
    report = dict(mode=args.mode, started_utc=datetime.now(timezone.utc).isoformat(), results=[],
                  package=str(args.package), host_sha256=hashlib.sha256((args.package / 'butterpollo.exe').read_bytes()).hexdigest(),
                  limits={'fresh_fps_minimum_fraction': .9, 'window_seconds': 10},
                  skipped=[dict(name='controllers-vigem', reason='receiver fixture has no controller sender'),
                           dict(name='controllers-vhf', reason='receiver fixture has no controller sender'),
                           dict(name='secure-desktop', reason='no Windows desktop or security changes; isolated debug host injects access-lost instead')])
    write(run / 'result.json', report)
    print(f'Artifacts: {run}', flush=True)
    wait_idle(overlap_log=run / 'overlap.jsonl')
    # e2e uses 48523. Refuse a collision instead of talking to another runner.
    for port in (PORT - 5, PORT, PORT + 1, PORT + 21):
        with socket.socket() as check:
            check.bind(('127.0.0.1', port))
    fixture = SimpleNamespace(package=args.package, client=args.client, mode='1920x1080x60', codec='hevc',
                              config=['wgc_user_helper=true', 'pyrowave=true', 'lan_encryption_mode=2'])
    env, display, sink = prepare(fixture, run, PORT)
    write(run / 'display.json', display)
    kernel, job = create_job()
    owned = []

    def spawn(command, path, environment=env):
        output = path.open('w', encoding='utf-8')
        process = subprocess.Popen([str(p) for p in command], env=environment, cwd=run, stdout=output,
                                   stderr=subprocess.STDOUT, creationflags=subprocess.CREATE_NO_WINDOW)
        owned.append((process, output))
        if not kernel.AssignProcessToJobObject(job, int(process._handle)):
            process.kill(); process.wait(timeout=10)
            raise ctypes.WinError(ctypes.get_last_error())
        return process

    def stop(process):
        if process is not None and process.poll() is None:
            process.terminate()
            process.wait(timeout=10)

    admin = requests.Session(); admin.verify = False; admin.trust_env = False
    client = requests.Session(); client.verify = False; client.trust_env = False
    web, https = f'https://127.0.0.1:{PORT + 1}', f'https://127.0.0.1:{PORT - 5}'
    host_log = run / 'config/logs/butterpollo.log'
    paired = None
    resources = []
    try:
        def start_host(binary, environment, name):
            offset = host_log.stat().st_size if host_log.exists() else 0
            process = spawn([binary, '--config-dir', run / 'config', '--assets',
                            args.package / 'assets/web', '--bind', '127.0.0.1', '--no-tray'], run / name, environment)
            deadline = time.monotonic() + 90
            while not (host_log.exists() and b'encoder capability probe completed' in host_log.read_bytes()[offset:]):
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError('isolated host did not become ready within 90 seconds')
                time.sleep(.2)
            login = admin.post(web + '/api/auth/login', json={'username': 'test', 'password': 'rust-smoke-only'}, timeout=10)
            login.raise_for_status(); admin.headers['X-CSRF-Token'] = login.json()['csrf_token']
            return process

        host = start_host(args.package / 'butterpollo.exe', env, 'host.stdout.log')
        baseline = snapshot(host.pid)
        write(run / 'baseline.json', baseline)

        def probes(folder, seconds, fps):
            tone = spawn([args.client.parent / 'audio_probe.exe', seconds + 60, sink['id'], '--render-only'], folder / 'tone.log')
            motion = spawn([args.client.parent / 'motion_probe.exe', display['display_name'], seconds + 60,
                            folder / 'motion.json', fps * 2, 128], folder / 'motion.log')
            time.sleep(.4)
            if tone.poll() is not None or motion.poll() is not None:
                raise RuntimeError('audio or motion fixture failed to start')
            return tone, motion

        prime = run / 'pair'; prime.mkdir()
        tone, motion = probes(prime, 10, 60)
        client_env = receiver_environment(env, display, fixture.mode, args.client, port=PORT)
        client_env['BUTTERPOLLO_TEST_KEEP_PAIRED'] = '1'
        pairing = spawn([sys.executable, ROOT.parent / 'tests/interop.py', prime, 'hevc', '1920', '1080', '60', '8', '20000', '4'],
                        prime / 'client.log', client_env)
        try:
            pairing.wait(timeout=75)
            paired = json.loads((prime / 'pairing.json').read_text())
        finally:
            stop(pairing); stop(motion); stop(tone)
        client.cert = (str(prime / 'client.pem'), str(prime / 'client-key.pem'))
        common = dict(appid=paired['appid'], rikey=bytes(range(16)).hex(), rikeyid='123', corever='1')
        last_disconnect = None
        settled_baseline = snapshot(host.pid)
        fault_dir = run / 'faults'
        for case in cases(args.mode):
            if case.get('resume'):
                installed_idle()
            else:
                wait_idle(host.pid, run / 'overlap.jsonl')
            if case.get('fault') and case['fault'] != 'wgc-helper' and not fault_dir.exists():
                debug_host = ROOT.parents[1] / 'target/qa/debug/butterpollo.exe'
                if not debug_host.is_file():
                    report['skipped'].append(dict(name=case['name'], reason='build the debug host for isolated fault injection'))
                    write(run / 'result.json', report)
                    continue
                stop(host)
                fault_dir.mkdir()
                host = start_host(debug_host, dict(env, BUTTERPOLLO_TEST_FAULT_DIR=str(fault_dir)), 'fault-host.stdout.log')
                report['fault_host_sha256'] = hashlib.sha256(debug_host.read_bytes()).hexdigest()
            folder = run / case['name']; folder.mkdir()
            write(folder / 'installed-before.json', dict(utc=datetime.now(timezone.utc).isoformat(),
                  log_tail=INSTALLED_LOG.read_text(errors='replace').splitlines()[-5:]))
            print(f"START {case['name']} ({case['seconds']} seconds)", flush=True)
            codec = case['codec']; width, height, fps = map(int, case['mode'].split('x'))
            executable = args.client.parent / 'moonlight-pyrowave-client.exe' if codec.startswith('pyrowave') else args.client
            if not executable.is_file():
                report['skipped'].append(dict(name=case['name'], reason=f'missing {executable.name}'))
                write(run / 'result.json', report)
                continue
            result = dict(case, passed=False, failures=[])
            receiver = tone = motion = load = None
            samples = []
            injected_at = None
            repeated_fault = False
            log_start = host_log.stat().st_size
            try:
                response = admin.patch(web + '/api/config', json={'minimum_fps_target': str(fps)}, timeout=10)
                response.raise_for_status()
                tone, motion = probes(folder, case['seconds'], fps)
                client_env = receiver_environment(env, display, case['mode'], executable, case.get('vrr', False), PORT)
                client_env.update(BUTTERPOLLO_TEST_TIMING_CSV=str(folder / 'video.csv'),
                                  BUTTERPOLLO_TEST_AUDIO_CSV=str(folder / 'audio.csv'),
                                  BUTTERPOLLO_TEST_EXPECT_HDR_CONTROL='auto')
                if case.get('churn'):
                    client_env['BUTTERPOLLO_TEST_WARMUP_SECONDS'] = '.5'
                    client_env.pop('BUTTERPOLLO_TEST_MIN_FPS', None)
                    client_env.pop('BUTTERPOLLO_TEST_AUDIO_TONE', None)
                endpoint = '/resume' if case.get('resume') else '/launch'
                launched = time.monotonic()
                launch = ET.fromstring(client.get(https + endpoint, params={**common, 'mode': case['mode'],
                    'hdrMode': '1' if '-hdr' in codec else '0', 'clientVrrRequested': '1' if case.get('vrr') else '0'}, timeout=15).text)
                if launch.attrib.get('status_code') != '200':
                    raise RuntimeError(f'{endpoint} failed: {ET.tostring(launch, encoding="unicode")}')
                if case.get('resume') and last_disconnect is not None:
                    result['reconnect_seconds'] = launched - last_disconnect
                url = launch.findtext('sessionUrl0')
                if not url:
                    raise RuntimeError(f'{endpoint} returned no session URL')
                if case.get('load'):
                    load = spawn([args.gpu_load, case['seconds'] + 30, 1000, 0, 200], folder / 'gpu-load.log')
                started = time.monotonic()
                receiver = spawn([executable, url, codec, width, height, fps, case['seconds'],
                                  400000 if codec.startswith('pyrowave') else 20000, 4], folder / 'client.log', client_env)
                next_sample = next_idle = 0
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as sampler, (folder / 'samples.jsonl').open('w') as output:
                    pending = None
                    while receiver.poll() is None:
                        elapsed = time.monotonic() - started
                        if elapsed > case['seconds'] + 30:
                            raise TimeoutError('receiver exceeded its stream and teardown deadline')
                        if elapsed >= next_idle:
                            installed_idle(); next_idle = elapsed + 2
                        stats = admin.get(web + '/api/rtsp/sessions', timeout=3)
                        stats.raise_for_status()
                        sample = dict(elapsed_seconds=elapsed, **stats.json())
                        for stream in sample.get('sessions', []):
                            stream.get('performance', {}).pop('history', None)
                        if pending is not None and pending.done():
                            sample['resources'] = pending.result(); pending = None
                        if not case.get('churn') and elapsed >= next_sample and pending is None:
                            pending = sampler.submit(snapshot, host.pid); next_sample = elapsed + 5
                        samples.append(sample); output.write(json.dumps(sample) + '\n'); output.flush()
                        if case.get('fault') and injected_at is None and elapsed >= 7:
                            installed_idle()
                            state = snapshot(host.pid)
                            if case['fault'] == 'wgc-helper':
                                helpers = [p for p in state['processes'] if p['ParentProcessId'] == host.pid and '--wgc-worker' in (p['CommandLine'] or '')]
                                if len(helpers) != 1:
                                    raise RuntimeError(f'expected one owned WGC helper, found {len(helpers)}')
                                subprocess.run(['taskkill', '/PID', str(helpers[0]['ProcessId']), '/F'], check=True,
                                               capture_output=True, timeout=10, creationflags=subprocess.CREATE_NO_WINDOW)
                            else:
                                (fault_dir / case['fault']).touch()
                            injected_at = time.monotonic() - started
                            result['injection'] = dict(elapsed_seconds=injected_at, fault=case['fault'])
                        if case.get('fault') == 'encoder failure' and injected_at is not None and elapsed >= injected_at + 4 and not repeated_fault:
                            (fault_dir / case['fault']).touch()
                            repeated_fault = True
                        if host.poll() is not None:
                            raise RuntimeError('isolated host exited during the stream')
                        if tone.poll() is not None or motion.poll() is not None or (load is not None and load.poll() is not None):
                            raise RuntimeError('a source or GPU load fixture exited during the stream')
                        time.sleep(.25)
                    if pending is not None:
                        samples.append(dict(elapsed_seconds=time.monotonic() - started, resources=pending.result()))
                last_disconnect = time.monotonic()
                text = (folder / 'client.log').read_text(errors='replace')
                host_text = host_log.read_bytes()[log_start:].decode(errors='replace')
                result['host_messages'] = [line for line in host_text.splitlines() if re.search(r'\b(WARN|ERROR)\b', line)]
                if case.get('fault'):
                    if injected_at is None:
                        raise RuntimeError('receiver ended before fault injection')
                    result.update(fault_outcome(samples, injected_at, receiver.returncode, host_text + '\n' + text))
                    if not result['passed']:
                        result['failures'].append(result['outcome'])
                    if case['fault'] == 'encoder failure' and not any(s.get('warning') for sample in samples for s in sample.get('sessions', [])):
                        result['passed'] = False; result['failures'].append('repeated encoder fallback was not surfaced to the console')
                else:
                    measured = evaluate(text + ('\nINTEROPERABILITY PASS\n' if receiver.returncode == 0 else ''),
                                        receiver.returncode, codec, case['mode'], case.get('vrr', False),
                                        tone_log=(folder / 'tone.log').read_text(errors='replace'))
                    result['receiver'] = measured
                    if case.get('churn'):
                        result['passed'] = (receiver.returncode == 0 and (measured['decoded'] or 0) > 30
                                            and measured['decoded'] == measured['frames'] and measured['decode_failures'] == 0
                                            and (measured['audio_packets'] or 0) > 0 and (measured['audio_peak'] or 0) > .001)
                        if not result['passed']:
                            result['failures'].append('churn session did not decode moving video and audible audio')
                    else:
                        result['passed'] = measured['passed']
                        result['failures'].extend(measured['failures'])
                    if any('ERROR' in line for line in result['host_messages']):
                        result['passed'] = False; result['failures'].append('host logged an error')
                result['video_windows'] = video_windows(folder / 'video.csv', fps, .5 if case.get('churn') else 3)
                result['audio_windows'] = audio_windows(folder / 'audio.csv')
                if not result['video_windows'] or not result['audio_windows']:
                    result['passed'] = False; result['failures'].append('missing time-series measurements; rebuild the receiver')
                for window in result['video_windows']:
                    if not case.get('fault') and window['seconds'] >= 5 and (window['fresh_fps'] < fps * .9 or window['picture_coverage'] < .95):
                        result['passed'] = False; result['failures'].append('a video window lost fresh pictures')
                        break
            except Exception as error:
                result['passed'] = False; result['failures'].append(f'{type(error).__name__}: {error}')
            finally:
                stop(receiver); stop(load); stop(motion); stop(tone)
                if case.get('fault'):
                    (fault_dir / case['fault']).unlink(missing_ok=True)
                # Keep the application for /resume; cancel after every second
                # churn session to exercise both teardown paths on one host.
                if not case.get('churn') or case.get('resume') or not result['passed']:
                    try:
                        response = client.get(https + '/cancel', timeout=10)
                        if ET.fromstring(response.text).attrib.get('status_code') != '200':
                            raise RuntimeError(response.text)
                    except Exception as error:
                        result['passed'] = False; result['failures'].append(f'cancel: {error}')
                try:
                    deadline = time.monotonic() + 10
                    while admin.get(web + '/api/rtsp/sessions', timeout=3).json().get('sessions'):
                        if time.monotonic() > deadline:
                            raise TimeoutError('session did not drain within 10 seconds')
                        time.sleep(.1)
                    # Do not put inventory queries between disconnect and the
                    # immediate resume; those queries can take several seconds.
                    if not case.get('churn') or case.get('resume'):
                        state = snapshot(host.pid)
                        result['settled'] = state
                        if case.get('churn'):
                            resources.append(state)
                        old_devices = {d['DeviceID'] for d in settled_baseline['devices']}
                        extra = [d for d in state['devices'] if d['DeviceID'] not in old_devices]
                        helpers = [p for p in state['processes'] if p['ParentProcessId'] == host.pid and '--wgc-worker' in (p['CommandLine'] or '')]
                        orphaned_slots = sorted(set(state['xinput_slots']) - set(settled_baseline['xinput_slots']))
                        if extra or helpers or orphaned_slots:
                            result['passed'] = False; result['failures'].append('devices or capture helpers remained after teardown')
                            result['leftover_devices'] = extra; result['leftover_helpers'] = helpers
                            result['orphaned_slots'] = orphaned_slots
                except Exception as error:
                    result['passed'] = False; result['failures'].append(f'teardown: {error}')
                write(folder / 'result.json', result)
                report['results'].append(result)
                write(run / 'result.json', report)
                print(('PASS ' if result['passed'] else 'FAIL ') + case['name'] + ': ' + '; '.join(result['failures']), flush=True)
        report['resource_growth'] = resource_growth(resources)
        reconnects = [r['reconnect_seconds'] for r in report['results'] if 'reconnect_seconds' in r]
        report['quick_reconnect'] = dict(passed=bool(reconnects) and min(reconnects) <= 2, seconds=reconnects)
    except BaseException as error:
        report['fatal'] = f'{type(error).__name__}: {error}'
        print(report['fatal'], flush=True)
    finally:
        if paired:
            try:
                client.get(https + '/cancel', timeout=5)
                admin.post(web + '/api/clients/unpair', json={'uuid': paired['uuid']}, timeout=5).raise_for_status()
            except Exception as error:
                report['cleanup_error'] = str(error)
        kernel.CloseHandle(job)
        for process, output in reversed(owned):
            process.wait(timeout=10); output.close()
        report['finished_utc'] = datetime.now(timezone.utc).isoformat()
        report['passed'] = (len(report['results']) == len(cases(args.mode)) and all(r['passed'] for r in report['results'])
                            and report.get('resource_growth', {}).get('passed', False)
                            and report.get('quick_reconnect', {}).get('passed', False)
                            and not report.get('fatal') and not report.get('cleanup_error'))
        write(run / 'result.json', report)
    print(f"{'PASS' if report['passed'] else 'FAIL'}: {run / 'result.json'}", flush=True)
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
