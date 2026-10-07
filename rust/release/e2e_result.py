"""Acceptance checks for the independent receiver's release-test measurements."""
import re


def evaluate(client, rc, codec, mode, vrr=False):
    def find(pattern, cast=float):
        match = re.search(pattern, client)
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
    result.update(passed=not failures, failures=failures)
    return result
