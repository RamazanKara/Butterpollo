import unittest
from e2e_result import evaluate


GOOD = '''RESULT frames=1800 decoded_frames=1800 audio_packets=6000 failures=0
AUDIO_SIGNAL samples=2880000 peak=0.050000 rms=0.035000
AUDIO_TONE blocks=11000 min_rms=0.034000 max_rms=0.036000 continuous=1
STEADY warmup_seconds=3.000 seconds=27.000 frames=1621 fps=60.000 intervals_over_1_5_period=0
STEADY_HOST samples=1621 mean_ms=1.500 p50_ms=1.500 p95_ms=1.700 p99_ms=2.000 max_ms=2.500
ARRIVAL_INTERVAL samples=1621 mean_ms=16.667 p50_ms=16.667 p95_ms=17.000 p99_ms=18.000 max_ms=20.000
VISUAL frames=1621 unique=1621 repeats=0 skipped_render_frames=1620 unique_fps=60.000 coverage=1.000000
INTEROPERABILITY PASS
'''
PYROWAVE = GOOD + '''PYROWAVE framing=records bitstream=186f0393 encrypted=1 record_frames=1800 partial_frames=0 hdr_frames=0
PICTURE_AGE samples=1621 mean_ms=12.000 p50_ms=11.000 p95_ms=18.000 p99_ms=22.000 max_ms=30.000
'''


class ReleaseMeasurements(unittest.TestCase):
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

    def test_silence_and_audio_gaps_fail(self):
        for text in (GOOD.replace('peak=0.050000', 'peak=0.000000'), GOOD.replace('continuous=1', 'continuous=0')):
            self.assertFalse(evaluate(text, 0, 'hevc', '1280x720x60')['passed'])

    def test_receiver_and_decode_failures_fail(self):
        self.assertFalse(evaluate(GOOD, 1, 'hevc', '1280x720x60')['passed'])
        self.assertFalse(evaluate(GOOD.replace('failures=0', 'failures=1'), 0, 'hevc', '1280x720x60')['passed'])

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


if __name__ == '__main__':
    unittest.main()
