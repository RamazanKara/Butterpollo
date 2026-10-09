"""Workload driver for amf_sweep.ps1. The PowerShell parent owns every safety gate."""
import argparse, hashlib, json, os, pathlib, re, shutil, subprocess, sys, zipfile
from amf_sweep_result import ARMS, picture_metrics, quality_rows, read_runs, score_metrics, select_candidates, summarize

ROOT = pathlib.Path(__file__).resolve().parents[2]
ARTIFACT = pathlib.Path(r'C:\Users\ramaz\.codex\artifacts\butterpollo-rust-20260930')
BENCH = ARTIFACT / 'bench-rc21'
TARGET = pathlib.Path(r'D:\bp-build\amfsweep-target')
FFMPEG = pathlib.Path(r'C:\Users\ramaz\AppData\Local\Microsoft\WinGet\Links\ffmpeg.exe')
CLIPS = [pathlib.Path(r'D:\games\HomeBrew-Ports\snap') / name for name in
         ('Teenage Mutant Ninja Turtles HD.mp4', 'GoldenEye XBLA.mp4')]
PROFILES = [('1080p60', 1920, 1080, 60, 20000), ('1440p120', 2560, 1440, 120, 50000),
            ('native120', 1968, 2184, 120, 80000)]
FRAMES = 32
QUALITY_SECONDS = 2.2
MOTION_SECONDS = 22
LIMIT_SECONDS = 88 * 60


def settings(codec):
    rows = [
        (1, 'amd_max_frame_size', ['4', '2']),
        (2, 'amd_ltr_frames', ['1', '2']),
        (3, 'amd_vbaq', ['enabled' if codec == 'h264' else 'disabled']),
        (4, 'amd_rc', ['vbr_peak', 'cbr']),
        (4, 'amd_peak_bitrate_ratio', ['1', '1.5']),
        (4, 'amd_vbv_buffer_frames', ['2', '1']),
        (4, 'amd_enforce_hrd', ['true']),
    ]
    if codec == 'av1':
        rows += [(5, 'amd_av1_tiles', ['1', '2', '4']), (5, 'amd_av1_screen_content', ['disabled', 'enabled'])]
    rows += [(6, 'amd_input_queue_size', ['4', '2', '1']), (6, 'QueryTimeout', ['0']),
             (7, 'amd_quality', ['balanced', 'quality']), (7, 'amd_high_motion_quality_boost', ['enabled', 'disabled'])]
    rows += [(8, 'amd_av1_latency_mode', ['lowest', 'realtime'])] if codec == 'av1' else [(8, 'amd_lowlatency_mode', ['enabled'])]
    if codec != 'h264':
        rows += [(8, 'amd_split_frame', ['disabled', 'enabled'])]
    return [(rank, key, value) for rank, key, values in rows for value in values]


def estimate():
    quality = 4 * 3 * sum(len(settings(codec)) + 1 for codec in ('hevc', 'av1', 'h264'))
    age = 4 * 3 * 3 * 3
    seconds = 10 * 60 + quality * QUALITY_SECONDS + age * MOTION_SECONDS
    return dict(quality_runs_max=quality, age_runs_max=age, frames_per_quality_run=FRAMES,
                estimate_minutes=round(seconds / 60, 1), hard_stop_minutes=LIMIT_SECONDS // 60,
                assumptions='10 min build/reference preparation, 2.2 s per quality run, 22 s per fixture; idle host and warm Cargo cache')


def gate(label, logs=()):
    print(json.dumps(dict(guard=label, logs=[str(p) for p in logs])), flush=True)
    if sys.stdin.readline().strip() != 'ok':
        raise RuntimeError('PowerShell safety controller did not authorize the next run')


def command(args, directory, name, *, env=None, stdin=None, timeout=60, check=True):
    directory.mkdir(parents=True, exist_ok=True)
    out, err = directory / (name + '.stdout.log'), directory / (name + '.stderr.log')
    (directory / (name + '.command.json')).write_text(json.dumps([str(a) for a in args]))
    gate(name, [out, err])
    with out.open('wb') as stdout, err.open('wb') as stderr:
        result = subprocess.run([str(a) for a in args], stdin=stdin, stdout=stdout, stderr=stderr,
                                cwd=directory, env=env, timeout=timeout,
                                creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    if check and result.returncode:
        raise RuntimeError(f'{name} exited {result.returncode}: {err.read_text(errors="replace")[-2000:]}')
    return result.returncode, out.read_text(errors='replace'), err.read_text(errors='replace')


def build(work):
    revision = subprocess.check_output(['git', '-C', ROOT, 'rev-parse', 'HEAD'], text=True).strip()
    cache = TARGET / 'sweep-bin' / revision
    if (cache / 'complete.json').exists():
        manifest = json.loads((cache / 'complete.json').read_text())
        if all((cache / name).is_file() and hashlib.sha256((cache / name).read_bytes()).hexdigest() == digest
               for name, digest in manifest.items()):
            return cache
    source = work / 'source'
    source.mkdir()
    command(['git', '-C', ROOT, 'archive', '--format=zip', '--output', work / 'source.zip', 'HEAD'], work, 'archive')
    with zipfile.ZipFile(work / 'source.zip') as archive:
        archive.extractall(source)
    patch = source / 'rust/windows/src/amf.rs'
    original = patch.read_text()
    before, after = 'e.property("QueryTimeout", int(1))', 'e.property("QueryTimeout", int(0))'
    if original.count(before) != 1:
        raise ValueError('QueryTimeout source changed; review the private build patch')
    manifest = {}
    for variant in ('default', 'query0'):
        patch.write_text(original if variant == 'default' else original.replace(before, after))
        command(['pwsh', '-NoProfile', '-File', r'C:\src\cargo-one.ps1', 'build', '--release', '--locked',
                 '--manifest-path', source / 'Cargo.toml', '-p', 'butterpollo-windows', '--example', 'amf_quality',
                 '-p', 'butterpollo', '--bin', 'butterpollo', '-j', '2'], work, 'build-' + variant, timeout=1200)
        destination = cache / variant
        destination.mkdir(parents=True, exist_ok=True)
        for name, binary in [('amf_quality.exe', TARGET / 'release/examples/amf_quality.exe'),
                             ('butterpollo.exe', TARGET / 'release/butterpollo.exe')]:
            shutil.copy2(binary, destination / name)
            manifest[f'{variant}/{name}'] = hashlib.sha256(binary.read_bytes()).hexdigest()
        for dll in pathlib.Path(r'C:\Program Files\ButterpolloRust').glob('*.dll'):
            shutil.copy2(dll, destination / dll.name)
    (cache / 'complete.json').write_text(json.dumps(manifest, indent=2))
    (work / 'build.json').write_text(json.dumps(dict(revision=revision, binaries=manifest, query_patch=[before, after]), indent=2))
    return cache


def prepare_reference(work, profile, hdr):
    name, width, height, fps, _ = profile
    directory = work / 'references' / (name + ('-hdr' if hdr else '-sdr'))
    if (directory / 'reference.yuv').exists():
        return directory
    directory.mkdir(parents=True)
    pixel = 'gbrpf32le' if hdr else 'bgra'
    source = f'scale=w={width}:h={height}:force_original_aspect_ratio=increase:flags=lanczos,crop={width}:{height}'
    source += ',format=gbrpf32le,zscale=tin=iec61966-2-1:t=linear:pin=709:p=709' if hdr else ',format=bgra'
    # A hard transition inside the clip exposes size caps without changing the content between arms.
    with (directory / 'input.raw').open('wb') as output:
        for index, clip in enumerate(CLIPS):
            part = directory / f'part{index}.raw'
            command([FFMPEG, '-v', 'error', '-threads', '2', '-ss', '3', '-i', clip, '-vf', source,
                     '-filter_threads', '2', '-frames:v', FRAMES // 2, '-pix_fmt', pixel, '-f', 'rawvideo', part],
                    directory, f'prepare-{index}', timeout=120)
            expected = width * height * (12 if hdr else 4) * (FRAMES // 2)
            if part.stat().st_size != expected:
                raise ValueError(f'{clip}: missing source pictures')
            with part.open('rb') as file:
                shutil.copyfileobj(file, output)
            part.unlink()
    conversion = ('zscale=tin=linear:pin=709:t=smpte2084:p=2020:m=2020_ncl:r=limited:npl=203,format=yuv420p10le'
                  if hdr else 'scale=out_color_matrix=bt601:out_range=tv,format=yuv420p')
    command([FFMPEG, '-v', 'error', '-f', 'rawvideo', '-pixel_format', pixel, '-video_size', f'{width}x{height}',
             '-framerate', fps, '-i', directory / 'input.raw', '-vf', conversion, '-filter_threads', '2',
             '-frames:v', FRAMES, '-f', 'rawvideo', directory / 'reference.yuv'], directory, 'reference', timeout=120)
    return directory


def quality(work, binaries, profile, codec, candidate, changes, arm, request):
    name, width, height, fps, target = profile
    hdr = codec != 'h264'
    comparison = f'{name}-{codec}-{candidate}'
    directory = work / 'quality' / comparison / arm
    directory.mkdir(parents=True)
    row = dict(kind='quality', comparison=comparison, profile=name, codec=codec, candidate=candidate,
               settings=changes, arm=arm, target_kbps=target, requested_kbps=request, fps=fps,
               hdr=hdr, status='failed', directory=str(directory))
    settings_now = changes if arm.startswith('B') else {}
    variant = 'query0' if settings_now.get('QueryTimeout') == '0' else 'default'
    config = directory / 'sunshine.conf'
    config.write_text('\n'.join(f'{k} = {v}' for k, v in settings_now.items() if k != 'QueryTimeout') + '\n')
    env = os.environ.copy()
    env.update(RUST_LOG='butterpollo_windows::amf=info', PATH=str(binaries / variant) + ';' + env['PATH'])
    try:
        reference = prepare_reference(work, profile, hdr)
        bits = directory / 'encoded.bin'
        with (reference / 'input.raw').open('rb') as file:
            rc, output, log = command([binaries / variant / 'amf_quality.exe', '--codec', codec, '--width', width,
                '--height', height, '--fps', fps, '--bitrate', request, '--frames', FRAMES,
                '--hdr', int(hdr), '--config', config, '--out', bits], directory, 'encode', env=env, stdin=file, timeout=15, check=False)
        if rc:
            # Unknown encoder failures may be GPU failures; never continue to another workload.
            raise OSError(f'encoder exited {rc}: {log[-3000:]}')
        log = re.sub(r'\x1b\[[0-9;]*m', '', log)
        metrics = json.loads(output.strip().splitlines()[-1])
        if metrics['encoded_frames'] != FRAMES or metrics['backend'] != 'amf':
            raise ValueError('probe did not encode every picture using native AMF')
        row.update(metrics)
        row['effective'] = '\n'.join(line for line in log.splitlines() if 'AMF encoder settings' in line or 'LTR' in line)
        if settings_now.get('amd_ltr_frames'):
            enabled = re.search(r'AMF long-term reference recovery enabled.*?count=(\d+)', log)
            if not enabled or int(enabled[1]) != int(settings_now['amd_ltr_frames']) or 'AMF LTR surface rejected' in log:
                raise ValueError('requested LTR count was not active; inspect readback in encode.stderr.log')
        fmt = dict(h264='h264', hevc='hevc', av1='obu')[codec]
        pixel = 'yuv420p10le' if hdr else 'yuv420p'
        graph = (f'[0:v]crop={width}:{height}:0:0,format={pixel},settb=1/{fps},setpts=N,split[d1][d2];'
                 f'[1:v]settb=1/{fps},setpts=N,split[r1][r2];'
                 '[d1][r1]psnr=stats_file=psnr.log:shortest=1[p];[d2][r2]ssim=stats_file=ssim.log:shortest=1[s]')
        _, _, score = command([FFMPEG, '-hide_banner', '-threads', '2', '-err_detect', 'explode', '-f', fmt, '-i', bits,
            '-f', 'rawvideo', '-pixel_format', pixel, '-video_size', f'{width}x{height}', '-framerate', fps,
            '-i', reference / 'reference.yuv', '-filter_complex_threads', '2', '-lavfi', graph,
            '-map', '[p]', '-map', '[s]', '-frames:v', FRAMES, '-f', 'null', '-'], directory, 'score', timeout=20)
        row.update(score_metrics(score, directory / 'psnr.log', directory / 'ssim.log', FRAMES), status='ok')
        geometry = re.search(r'Stream #0:0.*?Video:.*?\b(\d{3,5})x(\d{3,5})\b', score)
        if geometry:
            row.update(decoded_width=int(geometry[1]), decoded_height=int(geometry[2]))
    except (RuntimeError, ValueError, KeyError) as error:
        row['error'] = str(error)
    except (OSError, subprocess.TimeoutExpired) as error:
        row['error'] = str(error)
        append(work, row)
        raise
    append(work, row)
    return row


def append(work, row):
    with (work / 'runs.jsonl').open('a', encoding='utf-8') as file:
        file.write(json.dumps(row) + '\n')
    print(json.dumps(dict(result=f"{row['kind']} {row['comparison']} {row['arm']}: {row['status']}")), flush=True)


def quartet(work, binaries, profile, codec, key, value, context=None):
    changes = dict(context or {}, **{key: value})
    candidate = key + '=' + value
    if context:
        candidate += ',' + ','.join(f'{k}={v}' for k, v in context.items())
    a1 = quality(work, binaries, profile, codec, candidate, changes, 'A1', profile[4])
    b1 = quality(work, binaries, profile, codec, candidate, changes, 'B1', profile[4])
    quality(work, binaries, profile, codec, candidate, changes, 'A2', profile[4])
    request = profile[4]
    if a1['status'] == b1['status'] == 'ok':
        request = max(1000, min(200000, round(request * a1['actual_bitrate_kbps'] / b1['actual_bitrate_kbps'])))
    quality(work, binaries, profile, codec, candidate, changes, 'B2', request)


def fixture_source():
    path = BENCH / 'run-motion.py'
    source = path.read_text()
    # Preserve pairing/rendering/decoding/cleanup. Shorter helper lifetimes bound the sweep,
    # and replacement is essential because Config::parse keeps the first duplicate key.
    append_override = "    config += line.strip() + '\\n'"
    replace_override = ("    key = line.split('=', 1)[0].strip()\n"
                        "    config = '\\n'.join(item for item in config.splitlines() "
                        "if item.split('=', 1)[0].strip() != key) + '\\n' + line.strip() + '\\n'")
    if source.count("'40'") != 2 or source.count(append_override) != 1:
        raise ValueError('motion fixture changed; review the helper lifetime / config override adaptation')
    return path, source.replace("'40'", "'16'").replace(append_override, replace_override)


def motion(work, binaries, profile, codec, selected, arm):
    name, width, height, fps, target = profile
    candidate, changes = selected['candidate'], selected['settings']
    comparison = f'{name}-{codec}-{candidate}'
    directory = work / 'motion' / (comparison + '-wgc-' + arm)
    directory.mkdir(parents=True)
    row = dict(kind='age', comparison=comparison, profile=name, codec=codec, candidate=candidate, settings=changes,
               arm=arm, target_kbps=target, fps=fps, selection=selected['selection'], status='failed', directory=str(directory))
    settings_now = changes if arm.startswith('B') else {}
    variant = 'query0' if settings_now.get('QueryTimeout') == '0' else 'default'
    # The older motion fixture contains explicit tuning. Reset it to today's defaults before applying B.
    defaults = dict(capture='wgc', minimum_fps_target='20',
        virtual_display_layout='extended', amd_usage='ultralowlatency', amd_quality='speed', amd_rc='vbr_latency',
        amd_input_queue_size='0', amd_lowlatency_mode='auto', amd_av1_latency_mode='auto',
        amd_vbaq='disabled' if codec == 'h264' else 'enabled', amd_smart_access_video='auto', amd_split_frame='auto',
        amd_enforce_hrd='false', amd_ltr_frames='0', amd_preanalysis='false', amd_max_frame_size='0',
        amd_peak_bitrate_ratio='0', amd_vbv_buffer_frames='0', amd_av1_tiles='0', amd_av1_screen_content='auto',
        amd_high_motion_quality_boost='auto')
    defaults.update({k: v for k, v in settings_now.items() if k != 'QueryTimeout'})
    env = {k: v for k, v in os.environ.items() if not k.startswith(('BUTTERPOLLO_TEST_', 'AB_'))}
    env.update(BENCH_TOOLS=str(BENCH / 'tools'), BUTTERPOLLO_TEST_HOST_EXE=str(binaries / variant / 'butterpollo.exe'),
        BUTTERPOLLO_TEST_CLIENT_EXE=str(BENCH / 'moonlight-motion-client-hw.exe'), BUTTERPOLLO_TEST_HW_DECODER='d3d11va',
        BUTTERPOLLO_TEST_SIZE=f'{width}x{height}', BUTTERPOLLO_TEST_FPS=str(fps), BUTTERPOLLO_TEST_BITRATE=str(target),
        BUTTERPOLLO_TEST_SECONDS='12', BUTTERPOLLO_TEST_EXTRA_CONF=';'.join(f'{k} = {v}' for k, v in defaults.items()),
        BUTTERPOLLO_TEST_RFI='1', BUTTERPOLLO_TEST_POLL_SESSION_API='0',
        PATH=str(binaries / variant) + ';' + os.environ['PATH'])
    path, source = fixture_source()
    adapter = work / 'motion-fixture.py'
    if not adapter.exists():
        # Preserve __file__ and sys.path so the existing runner resolves its original helpers and assets.
        adapter.write_text('import sys\nsys.path.insert(0, ' + repr(str(BENCH)) + ')\n' +
            'exec(compile(' + repr(source) + ', ' + repr(str(path)) + ", 'exec'), dict(__name__='__main__', __file__=" + repr(str(path)) + '))\n')
        (work / 'motion-fixture.json').write_text(json.dumps(dict(source=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
            change="helper lifetimes: 40 -> 16 seconds; stream 12 s, warmup 5 s; replace duplicate configuration keys"), indent=2))
    gate('picture-age fixture', [directory / 'config/logs/butterpollo.log', directory / 'host.stderr.log',
                                directory / 'moonlight-interop.log', directory / 'renderer.log'])
    rc, _, _ = command([sys.executable, adapter, codec + ('-hdr' if codec != 'h264' else ''), directory,
                        'rust', 'virtual-motion'], directory, 'motion', env=env, timeout=60, check=False)
    client = directory / 'moonlight-interop.log'
    row.update(picture_metrics(client.read_text(errors='replace') if client.exists() else '', fps, rc), fixture_exit=rc)
    host_log = directory / 'config/logs/butterpollo.log'
    host_text = re.sub(r'\x1b\[[0-9;]*m', '', host_log.read_text(errors='replace')) if host_log.exists() else ''
    if settings_now.get('amd_ltr_frames'):
        enabled = re.findall(r'AMF long-term reference recovery enabled.*?count=(\d+)', host_text)
        if not enabled or int(enabled[-1]) != int(settings_now['amd_ltr_frames']) or 'AMF LTR surface rejected' in host_text:
            row.update(valid=False, error='stream did not enable the requested LTR count')
    row['status'] = 'ok' if row['valid'] else 'invalid'
    append(work, row)


def sweep(work):
    for path in [FFMPEG, *CLIPS, BENCH / 'run-motion.py', BENCH / 'interop.py', BENCH / 'installed_state.py',
                 BENCH / 'moonlight-motion-client-hw.exe', BENCH / 'tools/motion_probe.exe', BENCH / 'tools/audio_probe.exe',
                 ARTIFACT / 'test-python/Scripts/python.exe', ARTIFACT / 'target/butterpollo-rust-release/assets/web',
                 pathlib.Path(r'C:\src\cargo-one.ps1')]:
        if not path.exists():
            raise FileNotFoundError(f'required existing fixture/input: {path}')
    fixture_source()
    (work / 'plan.json').write_text(json.dumps(dict(estimate=estimate(), profiles=PROFILES,
        settings={c: settings(c) for c in ('hevc', 'av1', 'h264')}, clips=[str(p) for p in CLIPS],
        conditional_max_frame_size_1='after 4 or 2 reduces max bytes by 5%, with at most 0.002 SSIM loss'), indent=2))
    binaries = build(work)
    for rank in range(1, 9):
        for profile in PROFILES:
            for codec in ('hevc', 'av1', 'h264'):
                for _, key, value in (r for r in settings(codec) if r[0] == rank):
                    context = {}
                    if key in ('amd_peak_bitrate_ratio', 'amd_vbv_buffer_frames', 'amd_enforce_hrd'):
                        rc = [r for r in quality_rows(read_runs(work)) if r['profile'] == profile[0] and r['codec'] == codec
                              and r['candidate'].startswith('amd_rc=') and r['equal_actual'] and r['delta_ssim'] > 0]
                        if rc:
                            context = min(rc, key=lambda r: r['quality_rank'])['settings']
                    quartet(work, binaries, profile, codec, key, value, context)
                if rank == 1:
                    rows = read_runs(work)
                    useful = any(b['status'] == a['status'] == 'ok' and b['frame_bytes_max'] < a['frame_bytes_max'] * .95
                                 and b['ssim_all'] >= a['ssim_all'] - .002
                                 for a, b in zip(rows[::4], rows[1::4])
                                 if a['profile'] == profile[0] and a['codec'] == codec and 'amd_max_frame_size=' in a['candidate'])
                    if useful:
                        quartet(work, binaries, profile, codec, 'amd_max_frame_size', '1')
        summarize(work)
    selected = []
    for profile in PROFILES:
        for codec in ('hevc', 'av1', 'h264'):
            candidates = select_candidates(read_runs(work), profile[0], codec)
            selected.extend(candidates)
            for candidate in candidates:
                for arm in ARMS:
                    motion(work, binaries, profile, codec, candidate, arm)
            summarize(work)
    (work / 'selected.json').write_text(json.dumps(selected, indent=2))
    runs = read_runs(work)
    failed = sum(r['status'] != 'ok' for r in runs)
    (work / 'status.txt').write_text(f'Completed sweep: {len(runs)} runs, {failed} failed/invalid; {len(selected)}/27 streaming candidates.\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['estimate', 'run', 'summarize'])
    parser.add_argument('work', nargs='?', type=pathlib.Path)
    args = parser.parse_args()
    if args.action == 'estimate':
        print(json.dumps(estimate()))
    elif args.action == 'summarize':
        summarize(args.work)
    else:
        try:
            sweep(args.work)
        except BaseException as error:
            (args.work / 'status.txt').write_text(f'ABORTED: {type(error).__name__}: {error}\n')
            raise
        finally:
            summarize(args.work)
