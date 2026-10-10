import json, pathlib, subprocess, sys, tempfile, unittest
from unittest.mock import patch
from e2e_result import active_clients, evaluate, host_frames


GOOD = '''RESULT frames=1800 decoded_frames=1800 audio_packets=6000 failures=0
AUDIO_SIGNAL samples=2880000 peak=0.050000 rms=0.035000
AUDIO_TONE blocks=11000 min_rms=0.034000 max_rms=0.036000 continuous=1
STEADY warmup_seconds=3.000 seconds=27.000 frames=1621 fps=60.000 intervals_over_1_5_period=0
STEADY_HOST samples=1621 mean_ms=1.500 p50_ms=1.500 p95_ms=1.700 p99_ms=2.000 max_ms=2.500
ARRIVAL_INTERVAL samples=1621 mean_ms=16.667 p50_ms=16.667 p95_ms=17.000 p99_ms=18.000 max_ms=20.000
VISUAL frames=1621 unique=1621 repeats=0 skipped_render_frames=1620 unique_fps=60.000 coverage=1.000000
PERFORMANCE seconds=30.000 received_fps=60.00 decoded_fps=60.00 host_mean_ms=1.500 decoder_mean_ms=4.000
INTEROPERABILITY PASS
'''
# The strip moves at the stream rate here, so the receiver sees every frame.
RECOVERY = GOOD.replace('skipped_render_frames=1620', 'skipped_render_frames=0') + '''IDR_PROBE samples=16 mean_ms=12.000 p50_ms=11.000 p95_ms=20.000 max_ms=24.000 requested=16 sent=16 decoded=16
PICTURE_AGE samples=1621 mean_ms=12.000 p50_ms=11.000 p95_ms=18.000 p99_ms=22.000 max_ms=30.000
'''
# The host's per-claim trace (RUST_LOG=info,pacing=trace): new pictures only.
CLAIMS = ''.join(f'2026-10-09T10:00:00Z TRACE pacing: claim capture_id={i} presented={i * 16667} acquired={i * 16667 + 300} '
                 f'seen={i * 16667 + 400} claim={i * 16667 + 500} interval=16667 deadline=0 source_id=1 stream_id=e2e fresh=true\n'
                 for i in range(1, 700))
AGAIN = CLAIMS + (
    '2026-10-09T10:00:09Z TRACE pacing: claim capture_id=500 presented=8333500 acquired=8333800 seen=0 '
    'claim=8340000 interval=16667 deadline=0 source_id=1 stream_id=e2e fresh=false\n')
PYROWAVE = GOOD + '''PYROWAVE framing=records bitstream=186f0393 encrypted=1 record_frames=1800 partial_frames=0 hdr_frames=0
PICTURE_AGE samples=1621 mean_ms=12.000 p50_ms=11.000 p95_ms=18.000 p99_ms=22.000 max_ms=30.000
'''


class ReleaseMeasurements(unittest.TestCase):
    def test_source_refresh_query_uses_the_current_windows_mode_without_setting_it(self):
        from e2e_result import display_refresh
        def query(name, current, pointer):
            self.assertEqual((name, current), ('DISPLAY1', 0xffffffff))
            mode = pointer._obj
            self.assertEqual(mode.size, 220)
            self.assertEqual(type(mode).frequency.offset, 184)
            mode.frequency = 240
            return 1
        with patch('ctypes.windll', create=True) as native:
            native.user32.EnumDisplaySettingsW.side_effect = query
            self.assertEqual(display_refresh('DISPLAY1'), 240)
            native.user32.EnumDisplaySettingsW.assert_called_once()

    def test_capture_integrity_rejects_fallback_missing_and_changed_sources(self):
        source = dict(width=1968, height=2184, refresh_hz=240, pixel='RgbaF16')
        opened = 'INFO capture backend opened requested=wgc backend="wgc" output=DISPLAY1\n'
        configured = ('INFO stream configured width=1920 height=1080 requested_capture=wgc '
                      'source_width=1968 source_height=2184 source_refresh_hz=240 source_pixel=RgbaF16\n')
        check = lambda log: evaluate(GOOD, 0, 'hevc', '1920x1080x60', host_log=log,
                                     requested_capture='wgc', requested_source=source)
        matched = check(opened + configured)
        self.assertTrue(matched['passed'], matched['failures'])
        self.assertEqual(matched['comparison'], 'matched')
        self.assertEqual(matched['capture_sources'], [{k: str(v) for k, v in source.items()}])
        fallback = opened.replace('backend="wgc"', 'backend="ddx"')
        for log in (fallback + configured, opened + configured + fallback):
            result = check(log)
            self.assertFalse(result['passed'])
            self.assertEqual(result['comparison'], 'fallback')
            self.assertIn('ddx', result['capture_backends'])
        for log in ('', configured, opened, opened + configured.replace('source_pixel=RgbaF16', ''),
                    opened + configured + configured.replace('source_width=1968', 'source_width=1920')):
            self.assertFalse(check(log)['passed'])
        for key, wrong in [('width', '1920'), ('height', '1080'), ('refresh_hz', '120'), ('pixel', 'Bgra8')]:
            with self.subTest(key=key):
                self.assertFalse(check(opened + configured.replace(f'source_{key}={source[key]}', f'source_{key}={wrong}'))['passed'])
        alias = evaluate(GOOD, 0, 'hevc', '1920x1080x60', host_log=fallback + configured,
                         requested_capture='dxgi', requested_source=source)
        self.assertTrue(alias['passed'], alias['failures'])

    def test_picture_age_is_reported_for_every_codec(self):
        for codec in ('h264', 'hevc', 'av1', 'pyrowave'):
            result = evaluate(PYROWAVE, 0, codec, '1920x1080x60')
            self.assertTrue(result['passed'], result['failures'])
            self.assertEqual([result[f'picture_age_{key}_ms'] for key in ('mean', 'p95', 'p99')], [12, 18, 22])
        legacy = evaluate(GOOD, 0, 'hevc', '1920x1080x60')
        self.assertTrue(legacy['passed'])
        self.assertIsNone(legacy['picture_age_mean_ms'])
        self.assertEqual(legacy['comparison'], 'unverified')

    def test_one_disconnect_does_not_hide_another_active_client(self):
        log = ('INFO CLIENT CONNECTED client=phone\n'
               'INFO CLIENT CONNECTED client="living room"\n'
               'INFO CLIENT DISCONNECTED client="living room"\n')
        self.assertEqual(active_clients(log), ['phone'])
        self.assertEqual(active_clients(log + 'INFO CLIENT DISCONNECTED client=phone\n'), [])

    def test_repeated_names_and_log_rotation_do_not_hide_a_stream(self):
        log = ('INFO CLIENT DISCONNECTED client=phone\n'
               'INFO CLIENT CONNECTED client=phone\n'
               'INFO CLIENT CONNECTED client=phone\n'
               'INFO CLIENT DISCONNECTED client=phone\n')
        self.assertEqual(active_clients(log), ['phone'])

    def test_a_host_restart_ends_streams_it_never_logged_as_closed(self):
        log = ('INFO butterpollo::stream: CLIENT CONNECTED client=phone\n'
               'INFO butterpollo: Butterpollo Rust host started version="2.0.0"\n')
        self.assertEqual(active_clients(log), [])
        self.assertEqual(active_clients(log + 'INFO butterpollo::stream: CLIENT CONNECTED client=tv\n'), ['tv'])

    def test_validation_requires_1080p60_for_both_pyrowave_modes(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            work, package = root / 'work', root / 'package'
            package.mkdir()
            (package / 'butterpollo.exe').write_bytes(b'host')
            protocol = work / 'protocol'
            protocol.mkdir(parents=True)
            (protocol / 'result.json').write_text(json.dumps(dict(passed=True, checks=[])))
            cases = [('h264', False, 0), ('hevc', False, 0), ('av1', False, 0), ('hevc', True, 0),
                     ('hevc', False, 16), ('pyrowave', False, 0), ('pyrowave-hdr-444', False, 0)]
            for codec, vrr, recovery in cases:
                case = work / f'e2e-{codec}-{vrr}-{recovery}'
                case.mkdir()
                text = PYROWAVE.replace('hdr_frames=0', 'hdr_frames=1800') if '-hdr' in codec else PYROWAVE
                text = RECOVERY if recovery else text
                (case / 'result.json').write_text(json.dumps(
                    evaluate(text, 0, codec, '1920x1080x60', vrr, recovery=recovery, host_log=CLAIMS)))
            command = [sys.executable, str(pathlib.Path(__file__).with_name('validation.py')), '--version', 'test',
                       '--package', str(package), '--work', str(work), '--out', str(root / 'VALIDATION.json')]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            # The keyframe-request stream is required unless skipped on purpose.
            recovery = work / 'e2e-hevc-False-16'
            recovery.rename(work / 'held')
            run = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(run.returncode, 0)
            self.assertIn('keyframe-request', run.stderr)
            run = subprocess.run(command + ['--skipped', 'hevc-recovery'], capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            (work / 'held').rename(recovery)
            for codec in ('pyrowave', 'pyrowave-hdr-444'):
                path = work / f'e2e-{codec}-False-0' / 'result.json'
                original = path.read_text()
                for mode in ('1280x720x60', '1920x1080x30'):
                    with self.subTest(codec=codec, mode=mode):
                        result = json.loads(original)
                        result['mode'] = mode
                        path.write_text(json.dumps(result))
                        run = subprocess.run(command, capture_output=True, text=True)
                        self.assertNotEqual(run.returncode, 0)
                        self.assertIn('1080p60', run.stderr)
                path.write_text(original)

    def test_continuous_fixed_rate_and_vrr_streams_pass(self):
        for vrr in (False, True):
            for codec in ('h264', 'hevc', 'av1'):
                with self.subTest(vrr=vrr, codec=codec):
                    self.assertTrue(evaluate(GOOD, 0, codec, '1280x720x60', vrr)['passed'])

    def test_sixty_fps_in_pairs_still_fails_smoothness(self):
        paired = GOOD.replace('intervals_over_1_5_period=0', 'intervals_over_1_5_period=810')
        paired = paired.replace('p99_ms=18.000 max_ms=20.000', 'p99_ms=33.333 max_ms=33.333')
        self.assertFalse(evaluate(paired, 0, 'hevc', '1280x720x60')['passed'])

    def test_overspeed_and_underspeed_fail(self):
        for rate in ('54.000', '120.000'):
            with self.subTest(rate=rate):
                self.assertFalse(evaluate(GOOD.replace('fps=60.000', f'fps={rate}'), 0, 'hevc', '1280x720x60', True)['passed'])

    def test_one_large_stall_fails_even_with_good_p99(self):
        stalled = GOOD.replace('max_ms=20.000', 'max_ms=100.000')
        self.assertFalse(evaluate(stalled, 0, 'hevc', '1280x720x60')['passed'])

    def test_repeated_pictures_fail_even_with_regular_packets(self):
        repeated = GOOD.replace('unique_fps=60.000', 'unique_fps=30.000')
        self.assertFalse(evaluate(repeated, 0, 'hevc', '1280x720x60')['passed'])

    def test_missing_measurements_and_unreadable_motion_fail(self):
        for text in ('INTEROPERABILITY PASS', GOOD.replace('coverage=1.000000', 'coverage=0.500000')):
            self.assertFalse(evaluate(text, 0, 'hevc', '1280x720x60')['passed'])

    def test_decoder_time_is_reported_and_required(self):
        self.assertEqual(evaluate(GOOD, 0, 'hevc', '1920x1080x60')['decoder_mean_ms'], 4)
        result = evaluate(GOOD.replace('decoder_mean_ms=4.000', ''), 0, 'hevc', '1920x1080x60')
        self.assertIn('missing measurements: decoder_mean_ms', result['failures'])

    def test_silence_and_audio_gaps_fail(self):
        for text in (GOOD.replace('peak=0.050000', 'peak=0.000000'), GOOD.replace('continuous=1', 'continuous=0')):
            self.assertFalse(evaluate(text, 0, 'hevc', '1280x720x60')['passed'])

    def test_receiver_and_decode_failures_fail(self):
        self.assertFalse(evaluate(GOOD, 1, 'hevc', '1280x720x60')['passed'])
        self.assertFalse(evaluate(GOOD.replace('failures=0', 'failures=1'), 0, 'hevc', '1280x720x60')['passed'])

    def test_source_underruns_explain_but_never_excuse_audio_gaps(self):
        tone = 'AUDIO_RENDER buffer_frames=4800 sample_rate=48000\n'
        interrupted = GOOD.replace('continuous=1', 'continuous=0')
        for log, count in (('', None), (tone, 0),
                           (tone + 'AUDIO_RENDER_UNDERRUN elapsed_seconds=8.500\n', 1)):
            with self.subTest(underruns=count):
                result = evaluate(interrupted, 0, 'hevc', '1280x720x60', tone_log=log)
                self.assertFalse(result['passed'])
                self.assertEqual(result['audio_source_underruns'], count)
                self.assertIn('the captured audio tone was silent or interrupted', result['failures'])
                self.assertEqual(any('source also ran dry' in s for s in result['failures']), count == 1)

    def test_source_startup_does_not_override_measured_continuity(self):
        tone = ('AUDIO_RENDER buffer_frames=4800 sample_rate=48000\n'
                'AUDIO_RENDER_UNDERRUN elapsed_seconds=0.100\n')
        result = evaluate(GOOD, 0, 'hevc', '1280x720x60', tone_log=tone)
        self.assertTrue(result['passed'])
        self.assertEqual(result['audio_source_underruns'], 1)

    def test_keyframe_requests_must_ride_the_next_new_frame(self):
        moving = RECOVERY
        check = lambda client, log=CLAIMS: evaluate(client, 0, 'hevc', '1280x720x60', recovery=16, host_log=log)
        result = check(moving)
        self.assertTrue(result['passed'], result['failures'])
        self.assertEqual((result['host_claims'], result['host_pictures_encoded_again']), (699, 0))
        # The strip at the stream rate repeats and skips pictures by itself:
        # the receiver's counts are reported, the host's trace is judged.
        noisy = check(moving.replace('repeats=0', 'repeats=28').replace('skipped_render_frames=0', 'skipped_render_frames=25'))
        self.assertTrue(noisy['passed'], noisy['failures'])
        self.assertEqual((noisy['pictures_sent_again'], noisy['pictures_skipped']), (28, 25))
        again = check(moving, AGAIN)
        self.assertEqual(again['host_pictures_encoded_again'], 1)
        self.assertTrue(any('unchanged picture again' in f for f in again['failures']), again['failures'])
        untraced = check(moving, 'INFO stream timings fps=60\n')
        self.assertTrue(any('host_claims' in f for f in untraced['failures']), untraced['failures'])
        # A run whose RUST_LOG left out the trace is judged on the rest.
        unasked = evaluate(moving, 0, 'hevc', '1280x720x60', recovery=16,
                           host_log='INFO stream timings fps=60\n', claim_trace=False)
        self.assertTrue(unasked['passed'], unasked['failures'])
        self.assertIsNone(unasked['host_claims'])
        for before, after, failure in (
                ('samples=16 mean', 'samples=15 mean', 'no decoded keyframe'),
                ('decoded=16', 'decoded=15', 'no decoded keyframe'),
                ('p95_ms=20.000 max_ms=24.000', 'p95_ms=51.000 max_ms=60.000', 'three frame periods'),
                # One late frame per keyframe is allowed, beyond 1% of frames.
                ('intervals_over_1_5_period=0', 'intervals_over_1_5_period=32', None),
                ('p99_ms=18.000 max_ms=20.000', 'p99_ms=26.000 max_ms=34.000', None),
                ('intervals_over_1_5_period=0', 'intervals_over_1_5_period=33', 'excessive gaps'),
                ('p99_ms=18.000 max_ms=20.000', 'p99_ms=26.000 max_ms=51.000', 'excessive gaps'),
                ('IDR_PROBE', 'NO_PROBE', 'missing measurements')):
            with self.subTest(after=after):
                result = check(moving.replace(before, after))
                self.assertEqual(result['passed'], failure is None, result['failures'])
                if failure:
                    self.assertTrue(any(failure in f for f in result['failures']), result['failures'])
        # Without requests the gap rules are unchanged.
        self.assertFalse(evaluate(GOOD.replace('p99_ms=18.000 max_ms=20.000', 'p99_ms=26.000 max_ms=34.000'),
                                  0, 'hevc', '1280x720x60')['passed'])
        # Picture age is reported, not judged, and other streams ignore the probe.
        self.assertEqual(check(moving)['picture_age_p99_ms'], 22)
        self.assertNotIn('pictures_sent_again', evaluate(GOOD, 0, 'hevc', '1280x720x60'))

    def test_pyrowave_sdr_and_hdr_records_pass_with_picture_age(self):
        for codec, hdr in (('pyrowave', 0), ('pyrowave-hdr-444', 1800)):
            with self.subTest(codec=codec):
                result = evaluate(PYROWAVE.replace('hdr_frames=0', f'hdr_frames={hdr}'), 0, codec, '1920x1080x60')
                self.assertTrue(result['passed'], result['failures'])
                self.assertEqual(result['picture_age_mean_ms'], 12)

    def test_pyrowave_transport_only_or_codec_fallback_cannot_pass(self):
        for text in ('RESULT frames=1800 decoded_frames=1800 audio_packets=6000 failures=0\nINTEROPERABILITY PASS', GOOD):
            self.assertFalse(evaluate(text, 0, 'pyrowave', '1920x1080x60')['passed'])

    def test_pyrowave_requires_complete_encrypted_records_and_correct_hdr(self):
        for before, after in (('framing=records', 'framing=lengths'), ('bitstream=186f0393', 'bitstream=unknown'),
                              ('encrypted=1', 'encrypted=0'), ('record_frames=1800', 'record_frames=1799'),
                              ('partial_frames=0', 'partial_frames=1'), ('hdr_frames=0', 'hdr_frames=1800')):
            with self.subTest(after=after):
                self.assertFalse(evaluate(PYROWAVE.replace(before, after), 0, 'pyrowave', '1920x1080x60')['passed'])
        self.assertFalse(evaluate(PYROWAVE, 0, 'pyrowave-hdr-444', '1920x1080x60')['passed'])

    def test_pyrowave_requires_decoded_motion_tone_cadence_and_age(self):
        for before, after in (('unique_fps=60.000', 'unique_fps=30.000'), ('coverage=1.000000', 'coverage=0.500000'),
                              ('continuous=1', 'continuous=0'), ('decoded_frames=1800', 'decoded_frames=1799'),
                              ('p99_ms=18.000 max_ms=20.000', 'p99_ms=33.000 max_ms=100.000'),
                              ('PICTURE_AGE samples=1621', 'PICTURE_AGE samples=10'),
                              ('max_ms=30.000', 'max_ms=3001.000'), ('PICTURE_AGE', 'NO_PICTURE_AGE')):
            with self.subTest(after=after):
                self.assertFalse(evaluate(PYROWAVE.replace(before, after), 0, 'pyrowave', '1920x1080x60')['passed'])

    def test_pyrowave_fails_when_the_host_replaces_over_1_percent_of_frames(self):
        with tempfile.TemporaryDirectory() as folder:
            receiver = pathlib.Path(folder)
            self.assertEqual(host_frames(receiver), (None, None))
            samples = [dict(sessions=[]), dict(sessions=[dict(frames_sent=900, frames_replaced=3)]),
                       dict(sessions=[dict(frames_sent=1800, frames_replaced=18)])]
            (receiver / 'stream-pyrowave-1920-1080-60-threads4.json').write_text(json.dumps(dict(samples=samples)))
            self.assertEqual(host_frames(receiver), (1800, 18))
        for frames, passed in (((1800, 0), True), ((1800, 18), True), ((1800, 19), False), ((None, None), False)):
            with self.subTest(frames=frames):
                result = evaluate(PYROWAVE, 0, 'pyrowave', '1920x1080x60', host_frames=frames)
                self.assertEqual(result['passed'], passed, result['failures'])
        # Only e2e.py samples the host; soak runs and other codecs are unaffected.
        self.assertTrue(evaluate(PYROWAVE, 0, 'pyrowave', '1920x1080x60')['passed'])
        self.assertTrue(evaluate(GOOD, 0, 'hevc', '1280x720x60', host_frames=(None, None))['passed'])


if __name__ == '__main__':
    unittest.main()
