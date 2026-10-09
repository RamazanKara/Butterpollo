"""Acceptance checks for the independent receiver's release-test measurements."""
import json
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


def host_frames(receiver):
    """Frames the host sent and replaced unsent, from interop.py's last session sample."""
    for report in sorted(receiver.glob('stream-*.json')):
        sessions = [session for sample in json.loads(report.read_text()).get('samples', [])
                    for session in sample.get('sessions', [])]
        if sessions:
            return sessions[-1].get('frames_sent'), sessions[-1].get('frames_replaced')
    return None, None


def claims(log):
    """(claim, presented in microseconds since the stream began, new picture) per encode, from a host log with
    RUST_LOG=info,pacing=trace."""
    found = []
    for line in log.splitlines():
        if 'pacing' not in line or ' claim' not in line:
            continue
        fields = dict(re.findall(r'(\w+)=(\S+)', line))
        if 'claim' in fields and 'presented' in fields and 'fresh' in fields:
            found.append((int(fields['claim']), int(fields['presented']), fields['fresh'] == 'true'))
    return found


def evaluate(client, rc, codec, mode, vrr=False, tone_log='', host_frames=None, recovery=0, host_log=''):
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
        decoder_mean_ms=find(r'PERFORMANCE .*?decoder_mean_ms=([0-9.]+)'),
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
        if host_frames is not None:
            result.update(host_frames_sent=host_frames[0], host_frames_replaced=host_frames[1])
    if recovery:
        result.update(
            recovery_requests=recovery,
            recovery_keyframes=find(r'^IDR_PROBE samples=(\d+)', int),
            recovery_decoded=find(r'^IDR_PROBE .*?decoded=(\d+)', int),
            recovery_mean_ms=find(r'^IDR_PROBE .*?mean_ms=([0-9.]+)'),
            recovery_p95_ms=find(r'^IDR_PROBE .*?p95_ms=([0-9.]+)'),
            recovery_max_ms=find(r'^IDR_PROBE .*?max_ms=([0-9.]+)'),
        )
        encodes = claims(host_log)
        result.update(host_claims=len(encodes) or None,
                      host_pictures_encoded_again=sum(not new for *_, new in encodes) if encodes else None)
    failures = []
    missing = [key for key, value in result.items() if value is None]
    if recovery:
        # Reported for comparison between releases, not judged here. With the
        # strip at the stream rate the fixture alone repeats and skips 5-28
        # pictures in 12 s without any request (rc.28 and the fix, host
        # 2026-10-09), so the receiver cannot tell the host's doing apart.
        result.update(picture_age_p95_ms=find(r'^PICTURE_AGE .*?p95_ms=([0-9.]+)'),
                      picture_age_p99_ms=find(r'^PICTURE_AGE .*?p99_ms=([0-9.]+)'),
                      pictures_sent_again=find(r'^VISUAL .*?repeats=(\d+)', int),
                      pictures_skipped=find(r'^VISUAL .*?skipped_render_frames=(\d+)', int))
    if missing:
        failures.append('missing measurements: ' + ', '.join(missing))
    if rc != 0 or 'INTEROPERABILITY PASS' not in client:
        failures.append('receiver interoperability failed')
    if not missing:
        if result['frames'] < 30 or result['decoded'] != result['frames'] or result['decode_failures']:
            failures.append('video was not fully decoded')
        if result['steady_seconds'] < 5 or not fps * .97 <= result['steady_fps'] <= fps * 1.03:
            failures.append('steady frame rate is outside 97-103% of the requested rate')
        # A requested keyframe is several frames' worth of data and encode
        # time; one late frame per request is the keyframe's own cost.
        if (result['long_intervals'] > result['steady_frames'] * .01 + recovery
                or (not recovery and result['interval_p99_ms'] > 1500 / fps)
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
            # A replaced frame was still unsent when a newer one arrived.
            if host_frames is not None and result['host_frames_replaced'] > result['host_frames_sent'] * .01:
                failures.append('the host replaced more than 1% of PyroWave frames before sending them')
    if recovery and not missing:
        if result['recovery_keyframes'] != recovery or result['recovery_decoded'] != recovery:
            failures.append('a keyframe request got no decoded keyframe')
        # The motion strip moves every frame at the stream rate, so a request
        # rides the next new frame. rc.28 encoded the unchanged picture again
        # at once and let it take that frame's slot (10 of 16 requests); the
        # host's own claim trace shows it, the receiver's picture count does not.
        if result['host_pictures_encoded_again']:
            failures.append('the host encoded an unchanged picture again for a keyframe request')
        if result['recovery_p95_ms'] > 3000 / fps:
            failures.append('requested keyframes took longer than three frame periods')
    # Older fixtures cannot distinguish a starving tone source from host loss.
    underruns = (len(re.findall(r'^AUDIO_RENDER_UNDERRUN\b', tone_log, re.MULTILINE))
                 if re.search(r'^AUDIO_RENDER\b', tone_log, re.MULTILINE) else None)
    if result['audio_continuous'] == 0 and underruns:
        failures.append(f'test tone source also ran dry {underruns} time(s); CPU contention can interrupt the fixture')
    result.update(audio_source_underruns=underruns, passed=not failures, failures=failures)
    return result
