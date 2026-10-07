"""Acceptance checks for the independent receiver's release-test measurements."""
import re
from collections import Counter


def active_clients(log):
    active = Counter()
    for line in log.splitlines():
        # The log outlives the host: a stream it never closed ended with it.
        if 'Butterpollo Rust host started' in line:
            active.clear()
            continue
        event = re.search(r'CLIENT (CONNECTED|DISCONNECTED)\b.*?\bclient=(.*)', line)
        if event:
            action, client = event.groups()
            if action == 'CONNECTED':
                active[client] += 1
            elif active[client]:
                active[client] -= 1
    return sorted(client for client, count in active.items() if count)


def evaluate(client, rc, codec, mode, vrr=False, tone_log=''):
    def find(pattern, cast=float):
        match = re.search(pattern, client, re.MULTILINE)
        return cast(match.group(1)) if match else None

    fps = float(mode.split('x')[2])
    result = dict(
        codec=codec, mode=mode, vrr=vrr, client_exit=rc,
        frames=find(r'RESULT frames=(\d+)', int),
        decoded=find(r'decoded_frames=(\d+)', int),
        audio_packets=find(r'audio_packets=(\d+)', int),
        decode_failures=find(r'failures=(\d+)', int),
        steady_seconds=find(r'STEADY warmup_seconds=\S+ seconds=([0-9.]+)'),
        steady_frames=find(r'STEADY .*? frames=(\d+)', int),
        steady_fps=find(r'STEADY .*? fps=([0-9.]+)'),
        long_intervals=find(r'intervals_over_1_5_period=(\d+)', int),
        interval_p99_ms=find(r'ARRIVAL_INTERVAL .*?p99_ms=([0-9.]+)'),
        interval_max_ms=find(r'ARRIVAL_INTERVAL .*?max_ms=([0-9.]+)'),
        unique_fps=find(r'VISUAL .*?unique_fps=([0-9.]+)'),
        motion_coverage=find(r'VISUAL .*?coverage=([0-9.]+)'),
        audio_peak=find(r'AUDIO_SIGNAL .*?peak=([0-9.]+)'),
        audio_tone_blocks=find(r'AUDIO_TONE blocks=(\d+)', int),
        audio_continuous=find(r'AUDIO_TONE .*?continuous=(\d+)', int),
        host_mean_ms=find(r'STEADY_HOST .*?mean_ms=([0-9.]+)'),
        host_p99_ms=find(r'STEADY_HOST .*?p99_ms=([0-9.]+)'),
    )
    pyrowave = codec.startswith('pyrowave')
    if pyrowave:
        result.update(
            record_frames=find(r'^PYROWAVE framing=records bitstream=186f0393 encrypted=1 record_frames=(\d+)', int),
            partial_frames=find(r'^PYROWAVE .*?partial_frames=(\d+)', int),
            hdr_frames=find(r'^PYROWAVE .*?hdr_frames=(\d+)', int),
            picture_age_samples=find(r'^PICTURE_AGE samples=(\d+)', int),
            picture_age_mean_ms=find(r'^PICTURE_AGE .*?mean_ms=([0-9.]+)'),
            picture_age_p95_ms=find(r'^PICTURE_AGE .*?p95_ms=([0-9.]+)'),
            picture_age_p99_ms=find(r'^PICTURE_AGE .*?p99_ms=([0-9.]+)'),
            picture_age_max_ms=find(r'^PICTURE_AGE .*?max_ms=([0-9.]+)'),
        )
    failures = []
    missing = [key for key, value in result.items() if value is None]
    if missing:
        failures.append('missing measurements: ' + ', '.join(missing))
    if rc != 0 or 'INTEROPERABILITY PASS' not in client:
        failures.append('receiver interoperability failed')
    if not missing:
        if result['frames'] < 30 or result['decoded'] != result['frames'] or result['decode_failures']:
            failures.append('video was not fully decoded')
        if result['steady_seconds'] < 5 or not fps * .97 <= result['steady_fps'] <= fps * 1.03:
            failures.append('steady frame rate is outside 97-103% of the requested rate')
        if (result['long_intervals'] > result['steady_frames'] * .01
                or result['interval_p99_ms'] > 1500 / fps
                or result['interval_max_ms'] > 3000 / fps):
            failures.append('frame delivery has excessive gaps')
        if result['motion_coverage'] < .95 or result['unique_fps'] < fps * .9:
            failures.append('moving pictures were missing or repeated too often')
        if (not result['audio_packets'] or not result['audio_tone_blocks']
                or result['audio_peak'] <= .01 or result['audio_continuous'] != 1):
            failures.append('the captured audio tone was silent or interrupted')
        if pyrowave:
            if (result['record_frames'] != result['frames'] or result['partial_frames']
                    or result['hdr_frames'] != (result['frames'] if '-hdr' in codec else 0)):
                failures.append('PyroWave records were incomplete or the HDR mode was wrong')
            if (result['picture_age_samples'] < result['steady_frames'] * .95
                    or not 0 <= result['picture_age_mean_ms'] <= result['picture_age_max_ms'] <= 3000
                    or not 0 <= result['picture_age_p95_ms'] <= result['picture_age_p99_ms'] <= result['picture_age_max_ms']):
                failures.append('PyroWave picture age was missing or invalid')
    # Older fixtures cannot distinguish a starving tone source from host loss.
    underruns = (len(re.findall(r'^AUDIO_RENDER_UNDERRUN\b', tone_log, re.MULTILINE))
                 if re.search(r'^AUDIO_RENDER\b', tone_log, re.MULTILINE) else None)
    if result['audio_continuous'] == 0 and underruns:
        failures.append(f'test tone source also ran dry {underruns} time(s); CPU contention can interrupt the fixture')
    result.update(audio_source_underruns=underruns, passed=not failures, failures=failures)
    return result
