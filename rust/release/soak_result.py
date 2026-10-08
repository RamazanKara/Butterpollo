"""Offline checks for the soak's complete receiver and resource timelines."""
import csv, re, statistics


def percentile(values, percent):
    values = sorted(values)
    return values[(len(values) - 1) * percent // 100] if values else None


def video_windows(path, fps, warmup=3):
    windows = []
    bucket, start, previous, last_picture = [], None, None, 0

    def finish(rows):
        ages = [r['picture_age_ms'] for r in rows if r['picture_age_ms'] >= 0]
        elapsed = (rows[-1]['arrival_ms'] - rows[0]['arrival_ms']) / 1000
        windows.append(dict(elapsed_seconds=(rows[0]['arrival_ms'] - start) / 1000,
                            seconds=elapsed, frames=len(rows),
                            fresh_fps=sum(r['fresh'] for r in rows[1:]) / elapsed if elapsed else 0,
                            picture_age_p50_ms=percentile(ages, 50),
                            picture_age_p99_ms=percentile(ages, 99),
                            picture_coverage=len(ages) / len(rows),
                            arrival_stutters=sum(r['interval_ms'] > 2000 / fps for r in rows),
                            idr_frames=sum(r['frame_type'] == 1 for r in rows)))

    with path.open(newline='') as stream:
        for row in csv.DictReader(stream):
            row = {key: float(value) for key, value in row.items()}
            if start is None:
                start = row['arrival_ms']
            row['interval_ms'] = (row['assembled_us'] - previous) / 1000 if previous is not None else 0
            previous = row['assembled_us']
            row['fresh'] = row['render_frame'] != 0 and row['render_frame'] != last_picture
            last_picture = row['render_frame']
            if row['arrival_ms'] < start + warmup * 1000:
                continue
            if bucket and row['arrival_ms'] - bucket[0]['arrival_ms'] >= 10000:
                finish(bucket)
                bucket = []
            bucket.append(row)
    if bucket:
        finish(bucket)
    return windows


def audio_windows(path):
    windows, bucket = [], []
    start = None
    previous = None

    def finish(rows):
        checked = [r for r in rows if r['min_rms'] >= 0]
        low = min((r['min_rms'] for r in checked), default=None)
        high = max((r['max_rms'] for r in checked), default=None)
        windows.append(dict(elapsed_seconds=(rows[0]['arrival_ms'] - start) / 1000,
                            packets=len(rows), decoded_samples=sum(r['samples'] for r in rows),
                            arrival_gap_max_ms=max(r['gap'] for r in rows),
                            min_rms=low, max_rms=high,
                            continuous=bool(checked) and low >= high * .5 and high > .001))

    with path.open(newline='') as stream:
        for values in csv.DictReader(stream):
            row = {key: float(value) for key, value in values.items()}
            if start is None:
                start = row['arrival_ms']
            row['gap'] = row['arrival_ms'] - previous if previous is not None else 0
            previous = row['arrival_ms']
            if bucket and row['arrival_ms'] - bucket[0]['arrival_ms'] >= 10000:
                finish(bucket)
                bucket = []
            bucket.append(row)
    if bucket:
        finish(bucket)
    return windows


def resource_growth(samples):
    # Compare settled sessions after the first five warm the driver caches.
    samples = samples[5:]
    if len(samples) < 10:
        return dict(passed=False, failures=['fewer than 15 settled resource samples'])
    limits = dict(working_set=64 * 1024 * 1024, handles=16, threads=4)
    growth, failures = {}, []
    for key, limit in limits.items():
        values = [s[key] for s in samples]
        delta = statistics.median(values[-5:]) - statistics.median(values[:5])
        xmean = (len(values) - 1) / 2
        slope = sum((i - xmean) * value for i, value in enumerate(values)) / sum((i - xmean) ** 2 for i in range(len(values)))
        growth[key] = dict(first_median=statistics.median(values[:5]), last_median=statistics.median(values[-5:]),
                           delta=delta, per_cycle=slope, allowed_growth=limit)
        if delta > limit and slope > 0:
            failures.append(f'{key} grows after warmup: {delta:g} ({slope:.2f}/cycle)')
    return dict(passed=not failures, failures=failures, measurements=growth)


def fault_outcome(samples, injected_at, client_exit, log, windows, fps):
    before = [s for s in samples if s['elapsed_seconds'] <= injected_at and s.get('sessions')]
    if not before:
        return dict(passed=False, outcome='fault did not interrupt an established stream')
    warning = re.search(r'\bcode="(?:capture_recovery|encoder_recovery|encoder_compute_recovery)"', log)
    warning = warning or re.search(r'capture restarting|encoding failed', log, re.IGNORECASE)
    # The host allows 20 seconds to recover; a complete ten-second receiver
    # window must confirm fresh pictures by 30 seconds after the fault.
    for window in windows:
        observed = window['elapsed_seconds'] + window['seconds'] - injected_at
        if (warning and injected_at <= window['elapsed_seconds'] <= injected_at + 20
                and window['seconds'] >= 5 and observed <= 30
                and window['fresh_fps'] >= fps * .9 and window['picture_coverage'] >= .95):
            return dict(passed=True, outcome='recovered', recovery_observed_seconds=observed)
    if client_exit not in (None, 0) and 'session failed' in log and re.search(r'TERMINATED error=-?[1-9]\d*', log):
        return dict(passed=True, outcome='explicit stream error')
    return dict(passed=False, outcome='no bounded recovery or explicit stream error')
