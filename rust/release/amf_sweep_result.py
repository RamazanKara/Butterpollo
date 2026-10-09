"""Paired AMF quality and loopback rankings; no hardware access."""
import csv, json, math, pathlib, re
from collections import defaultdict

BITRATE_TOLERANCE = .03
ARMS = ('A1', 'B1', 'A2', 'B2')


def read_runs(directory):
    path = pathlib.Path(directory) / 'runs.jsonl'
    if not path.exists():
        return []
    rows = []
    lines = path.read_text(encoding='utf-8').splitlines(keepends=True)
    for index, line in enumerate(lines):
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            # The safety controller can terminate the worker during its last append.
            if index != len(lines) - 1 or line.endswith('\n'):
                raise
    return rows


def score_metrics(log, psnr, ssim, frames):
    for path in (psnr, ssim):
        rows = path.read_text().splitlines()
        if len(rows) != frames or [int(re.search(r'\bn:(\d+)', r)[1]) for r in rows] != list(range(1, frames + 1)):
            raise ValueError(f'{path.name}: reference/decode frame count mismatch')
    result = {}
    for prefix, pattern, names in (
            ('psnr', r'PSNR y:(\S+) u:(\S+) v:(\S+) average:(\S+)', ('y', 'u', 'v', 'average')),
            ('ssim', r'SSIM Y:(\S+) \([^)]*\) U:(\S+) \([^)]*\) V:(\S+) \([^)]*\) All:(\S+)', ('y', 'u', 'v', 'all'))):
        match = re.search(pattern, log)
        if not match:
            raise ValueError(f'missing {prefix} metrics')
        for name, value in zip(names, match.groups()):
            value = float(value)
            if math.isnan(value) or value < 0 or (prefix == 'ssim' and not 0 <= value <= 1):
                raise ValueError(f'invalid {prefix}_{name}')
            result[f'{prefix}_{name}'] = value
    return result


def picture_metrics(log, fps, exit_code):
    def value(pattern):
        match = re.search(pattern, log, re.MULTILINE)
        return float(match[1]) if match else None
    result = {f'picture_age_{p}_ms': value(r'^PICTURE_AGE .*?\b' + p + r'_ms=([0-9.]+)')
              for p in ('mean', 'p95', 'p99', 'max')}
    result.update(
        picture_age_samples=value(r'^PICTURE_AGE samples=(\d+)'),
        unique_fps=value(r'^VISUAL .*?unique_fps=([0-9.]+)'),
        coverage=value(r'^VISUAL .*?coverage=([0-9.]+)'),
        frames=value(r'^RESULT frames=(\d+)'),
        decoded_frames=value(r'^RESULT .*?decoded_frames=(\d+)'),
        decode_failures=value(r'^RESULT .*?failures=(\d+)'),
        decoder_mean_ms=value(r'^PERFORMANCE .*?decoder_mean_ms=([0-9.]+)'),
        host_mean_ms=value(r'^STEADY_HOST .*?mean_ms=([0-9.]+)'),
        host_p99_ms=value(r'^STEADY_HOST .*?p99_ms=([0-9.]+)'))
    required = [result[k] for k in ('picture_age_mean_ms', 'picture_age_p95_ms', 'picture_age_p99_ms',
                                   'picture_age_max_ms', 'picture_age_samples', 'unique_fps', 'coverage',
                                   'frames', 'decoded_frames', 'decode_failures', 'decoder_mean_ms')]
    valid = (exit_code == 0 and all(v is not None and math.isfinite(v) for v in required)
             and result['frames'] == result['decoded_frames'] and result['frames'] >= 30
             and result['decode_failures'] == 0 and result['coverage'] >= .95
             and result['picture_age_samples'] >= 30 and result['unique_fps'] >= fps * .95
             and result['decoder_mean_ms'] < 1000 / fps
             and 0 <= result['picture_age_mean_ms'] <= result['picture_age_max_ms']
             and 0 <= result['picture_age_p95_ms'] <= result['picture_age_p99_ms'] <= result['picture_age_max_ms'])
    return result | dict(valid=valid)


def quality_rows(runs):
    groups = defaultdict(list)
    for row in runs:
        if row['kind'] == 'quality':
            groups[row['comparison']].append(row)
    output = []
    for rows in groups.values():
        first = rows[0]
        result = {k: first[k] for k in ('comparison', 'profile', 'codec', 'candidate', 'settings', 'target_kbps', 'fps')}
        complete = tuple(r['arm'] for r in rows) == ARMS and all(r['status'] == 'ok' for r in rows)
        result.update(complete=complete, equal_actual=False, quality_rank=None, requested_rank=None)
        if complete:
            a1, b1, a2, b2 = rows
            result.update(requested_ssim=b1['ssim_all'], requested_psnr_y=b1['psnr_y'],
                          requested_delta_ssim=b1['ssim_all'] - a1['ssim_all'],
                          requested_actual_kbps=b1['actual_bitrate_kbps'],
                          default_actual_kbps=a2['actual_bitrate_kbps'],
                          matched_request_kbps=b2['requested_kbps'], actual_kbps=b2['actual_bitrate_kbps'],
                          bitrate_error=abs(b2['actual_bitrate_kbps'] / a2['actual_bitrate_kbps'] - 1),
                          ssim=b2['ssim_all'], psnr_y=b2['psnr_y'],
                          delta_ssim=b2['ssim_all'] - a2['ssim_all'],
                          delta_psnr_y=0 if b2['psnr_y'] == a2['psnr_y'] else b2['psnr_y'] - a2['psnr_y'],
                          control_drift_ssim=a2['ssim_all'] - a1['ssim_all'])
            for key in ('encode_mean_ms', 'encode_p95_ms', 'encode_p99_ms', 'frame_bytes_mean',
                        'frame_bytes_p99', 'all_frame_bytes_p99', 'frame_bytes_max', 'idr_bytes'):
                result[key] = b2[key]
            result['equal_actual'] = abs(b2['actual_bitrate_kbps'] - a2['actual_bitrate_kbps']) <= a2['actual_bitrate_kbps'] * BITRATE_TOLERANCE
            result['within_encode_budget'] = b2['encode_p99_ms'] < 1000 / b2['fps']
        output.append(result)
    for key in {(r['profile'], r['codec']) for r in output}:
        group = [r for r in output if (r['profile'], r['codec']) == key and r['complete']]
        for index, row in enumerate(sorted(group, key=lambda r: (-r['requested_ssim'], -r['requested_psnr_y'], r['candidate'])), 1):
            row['requested_rank'] = index
        # Rank paired deltas: each candidate has its own adjacent default control.
        for index, row in enumerate(sorted((r for r in group if r['equal_actual']),
                key=lambda r: (-r['delta_ssim'], -r['delta_psnr_y'], r['encode_p99_ms'], r['candidate'])), 1):
            row['quality_rank'] = index
    return sorted(output, key=lambda r: (r['profile'], r['codec'], r['quality_rank'] or 999, r['candidate']))


def select_candidates(runs, profile, codec):
    rows = [r for r in quality_rows(runs) if r['profile'] == profile and r['codec'] == codec
            and r['complete'] and r['within_encode_budget']]
    matched = [r for r in rows if r['equal_actual']]
    other = sorted((r for r in rows if not r['equal_actual']), key=lambda r: r['requested_rank'])
    return [(r | dict(selection='equal actual bitrate' if r['equal_actual'] else 'equal requested bitrate; unmatched actual'))
            for r in (matched + other)[:3]]


def age_rows(runs):
    groups = defaultdict(list)
    for row in runs:
        if row['kind'] == 'age':
            groups[row['comparison']].append(row)
    output = []
    for rows in groups.values():
        first = rows[0]
        result = {k: first[k] for k in ('comparison', 'profile', 'codec', 'candidate', 'settings', 'target_kbps')}
        result.update(valid=tuple(r['arm'] for r in rows) == ARMS and all(r.get('valid', False) for r in rows), age_rank=None)
        if result['valid']:
            controls, candidates = rows[::2], rows[1::2]
            for metric in ('picture_age_mean_ms', 'picture_age_p95_ms', 'picture_age_p99_ms', 'unique_fps', 'decoder_mean_ms'):
                result[metric] = sum(r[metric] for r in candidates) / 2
                result['default_' + metric] = sum(r[metric] for r in controls) / 2
            result['delta_p99_ms'] = result['picture_age_p99_ms'] - result['default_picture_age_p99_ms']
        output.append(result)
    for key in {(r['profile'], r['codec']) for r in output}:
        group = (r for r in output if (r['profile'], r['codec']) == key and r['valid'])
        for index, row in enumerate(sorted(group, key=lambda r: (r['delta_p99_ms'], r['picture_age_mean_ms'])), 1):
            row['age_rank'] = index
    return sorted(output, key=lambda r: (r['profile'], r['codec'], r['age_rank'] or 999, r['candidate']))


def write_csv(path, rows):
    fields = list(dict.fromkeys(k for row in rows for k in row))
    with path.open('w', newline='', encoding='utf-8') as file:
        writer = csv.DictWriter(file, fieldnames=fields)
        writer.writeheader()
        writer.writerows({k: json.dumps(v, sort_keys=True) if isinstance(v, (list, dict)) else v for k, v in row.items()} for row in rows)


def summarize(directory):
    directory = pathlib.Path(directory)
    runs = read_runs(directory)
    quality, ages = quality_rows(runs), age_rows(runs)
    write_csv(directory / 'runs.csv', runs)
    write_csv(directory / 'quality.csv', quality)
    write_csv(directory / 'picture-age.csv', ages)
    status = (directory / 'status.txt').read_text() if (directory / 'status.txt').exists() else 'INCOMPLETE'
    lines = ['# AMF sweep', '', status.strip(), '',
             'A1/B1 use the requested target; A2 uses defaults at that target. B2 adjusts the request using A1/B1 actual bitrate. '
             'Equal-actual ranks require agreement within 3% of A2; no score/bitrate division or extrapolation. '
             'Each quality point is one short encode, not two equal-bitrate repeats. Rank paired SSIM delta, then PSNR-Y delta, then encode p99.', '',
             '32 pictures concatenate 16 pictures from each existing game clip, including a scene cut. '
             'HEVC/AV1 use 10-bit PQ converted from SDR; H.264 uses SDR. These are not native HDR game highlights. '
             'PSNR/SSIM measure code-value fidelity, not HDR appearance. Short-clip p99 and top-three selection are screening evidence.', '',
             'Picture age uses the existing loopback motion/interop fixture with shorter helper lifetimes, WGC, extended layout and D3D11VA. '
             'All four streaming arms use the original requested target. Values are means of two per-run statistics. '
             'Age ranks use paired p99 reduction, then mean age. The adapter replaces duplicate config keys (the host keeps the first). '
             'Missing samples, strict interoperability failures (including AV1 padding), or decoder starvation are unranked. '
             'Age excludes scanout/input delay; no packet-loss, constrained-link or game-load claim is made. '
             'Serial quality submission cannot establish a queue-depth throughput gain. LTR uses the receiver RFI capability '
             '(unrestricted codec reference budget, as in the quality probe), intra refresh off, and checked LTR counts. '
             'VBV/peak/HRD candidates carry the selected RC in their label and settings; A still uses current defaults. '
             'Their deltas describe that combination, not the isolated extra knob.', '',
             'Max-frame=1 runs only if 4 or 2 lowers maximum frame bytes by at least 5% with at most 0.002 SSIM loss. '
             'Queue=1 is a separate fresh process. QueryTimeout=0 uses a private source build; no shipped config option changes.', '',
             '## Quality at matched actual bitrate', '',
             '| Profile / codec | Rank | Candidate | Actual kb/s | ΔSSIM | PSNR-Y | Encode p99 ms | Max / p99 P bytes |',
             '|---|---:|---|---:|---:|---:|---:|---:|']
    def fmt(value):
        return f'{value:.4f}' if isinstance(value, float) else str(value) if value is not None else '—'
    for row in quality:
        lines.append('| ' + ' | '.join(fmt(v) for v in (row['profile'] + ' / ' + row['codec'], row['quality_rank'],
            row['candidate'], row.get('actual_kbps'), row.get('delta_ssim'), row.get('psnr_y'), row.get('encode_p99_ms'),
            f"{row.get('frame_bytes_max', '—')} / {row.get('frame_bytes_p99', '—')}")) + ' |')
    lines += ['', 'Equal-requested ranks, unmatched actual rates, control drift, IDR sizes and every measurement are in quality.csv / runs.csv.', '',
              '## Picture age', '', '| Profile / codec | Rank | Candidate | Mean / p95 / p99 ms | Δp99 ms | Fresh fps |',
              '|---|---:|---|---:|---:|---:|']
    for row in ages:
        lines.append('| ' + ' | '.join(fmt(v) for v in (row['profile'] + ' / ' + row['codec'], row['age_rank'], row['candidate'],
            ' / '.join(fmt(row.get('picture_age_' + p + '_ms')) for p in ('mean', 'p95', 'p99')),
            row.get('delta_p99_ms'), row.get('unique_fps'))) + ' |')
    lines += ['', 'Safety observations: safety.jsonl. Per-run commands, configs, bitstreams, scoring logs and receiver artifacts are retained.', '']
    (directory / 'summary.md').write_text('\n'.join(lines), encoding='utf-8')
