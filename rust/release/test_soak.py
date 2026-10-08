import copy, csv, pathlib, tempfile, unittest
from soak_result import audio_windows, fault_outcome, load_limit, resource_growth, video_windows
from soak import cases


class SoakMeasurements(unittest.TestCase):
    def test_long_cases_retain_all_codecs_and_alternate_load(self):
        streams = cases('long')[:6]
        self.assertEqual([s['load'] for s in streams], [False, True, True, False, False, True])
        self.assertTrue(all(1200 <= s['seconds'] <= 1800 for s in streams))
        self.assertEqual(sum(s.get('churn', False) for s in cases('short')), 52)
        self.assertTrue(any(s.get('resume') for s in cases('short')))
        persistent = next(s for s in cases('short') if s.get('fault') == 'encoder failure.persistent')
        self.assertEqual(persistent['seconds'], 40)

    def test_windows_detect_a_late_freeze_beyond_the_old_receiver_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'frames.csv'
            with path.open('w', newline='') as stream:
                writer = csv.writer(stream)
                writer.writerow(['arrival_ms', 'assembled_us', 'picture_age_ms', 'render_frame', 'frame_type', 'decode_ms'])
                for i in range(108001):
                    writer.writerow([i * 1000 / 60, i * 1e6 / 60, 12 if i < 107000 else 200,
                                     min(i + 1, 107000), 1 if i == 0 else 2, 4])
            windows = video_windows(path, 60)
            self.assertGreater(windows[-1]['elapsed_seconds'], 1700)
            self.assertLess(windows[-1]['fresh_fps'], 1)
            self.assertEqual(windows[-1]['picture_age_p99_ms'], 200)
            self.assertEqual(windows[-1]['decoder_mean_ms'], 4)

    def test_two_period_arrival_gaps_are_counted_without_calling_them_send_gaps(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'frames.csv'
            path.write_text('arrival_ms,assembled_us,picture_age_ms,render_frame,frame_type,decode_ms\n'
                            '0,0,10,1,1,4\n20,20000,12,2,2,6\n60,60000,14,3,2,5\n')
            window = video_windows(path, 60, 0)[0]
            self.assertEqual(window['arrival_stutters'], 1)
            self.assertEqual(window['idr_frames'], 1)
            self.assertEqual(window['decoder_mean_ms'], 5)

    def test_only_decoder_starvation_limits_same_gpu_load(self):
        result = dict(load=True, mode='1920x1080x60', passed=False,
                      failures=['steady frame rate is outside 97-103% of the requested rate',
                                'moving pictures were missing or repeated too often',
                                'a video window lost fresh pictures'],
                      receiver=dict(client_exit=0, steady_seconds=21.6, motion_coverage=1,
                                    decoder_mean_ms=20),
                      video_windows=[dict(elapsed_seconds=3, seconds=10, picture_coverage=1,
                                          fresh_fps=49, decoder_mean_ms=20)])
        self.assertEqual(load_limit(result), 'same-GPU decoder starved by the load; measure with a separate client')
        for failure in ('receiver interoperability failed', 'video was not fully decoded',
                        'the captured audio tone was silent or interrupted', 'missing measurements: decoder_mean_ms',
                        'host logged an error', 'devices or capture helpers remained after teardown', 'cancel: failed'):
            with self.subTest(failure=failure):
                self.assertIsNone(load_limit(result | dict(failures=result['failures'] + [failure])))
        self.assertIsNone(load_limit(result | dict(load=False)))
        self.assertIsNone(load_limit(result | dict(passed=True, failures=[])))
        for change in (dict(client_exit=1), dict(steady_seconds=2), dict(motion_coverage=.9),
                       dict(decoder_mean_ms=None), dict(decoder_mean_ms=1000 / 60),
                       dict(decoder_mean_ms=6.942), dict(decoder_mean_ms=float('inf'))):
            with self.subTest(change=change):
                self.assertIsNone(load_limit(result | dict(receiver=result['receiver'] | change)))
        for change in (dict(picture_coverage=.9), dict(decoder_mean_ms=4), dict(decoder_mean_ms=None)):
            with self.subTest(window=change):
                mixed = copy.deepcopy(result)
                mixed['video_windows'].append(mixed['video_windows'][0] | change)
                self.assertIsNone(load_limit(mixed))

    def test_audio_silence_is_not_hidden_by_later_good_blocks(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'audio.csv'
            path.write_text('arrival_ms,samples,min_rms,max_rms\n0,240,-1,0\n'
                            '3000,240,0.03,0.032\n4000,240,0,0\n10001,240,0.03,0.032\n')
            windows = audio_windows(path)
            self.assertFalse(windows[0]['continuous'])
            self.assertTrue(windows[1]['continuous'])

    def test_resource_warmup_plateau_passes_but_per_cycle_leaks_fail(self):
        warmup = [dict(working_set=i * 2**20, handles=i * 100, threads=i * 5) for i in range(5)]
        stable = [dict(working_set=128 * 2**20, handles=500, threads=30) for _ in range(26)]
        self.assertTrue(resource_growth(warmup + stable)['passed'])
        leaking = [dict(working_set=(128 + i * 5) * 2**20, handles=500 + i * 2, threads=30 + i) for i in range(26)]
        result = resource_growth(warmup + leaking)
        self.assertFalse(result['passed'])
        self.assertEqual(len(result['failures']), 3)
        self.assertFalse(resource_growth(stable[:5])['passed'])

    def test_fault_requires_fresh_pictures_or_explicit_error(self):
        def sample(seconds, frames):
            return dict(elapsed_seconds=seconds, sessions=[dict(frames_sent=frames)])
        before = [sample(5, 300)]
        good = before + [sample(12, 600), sample(17, 900)]
        window = dict(elapsed_seconds=12, seconds=5, fresh_fps=60, picture_coverage=1)
        self.assertFalse(fault_outcome(good, 7, 0, 'capture restarting', [], 60)['passed'])
        self.assertTrue(fault_outcome(good, 7, 0, 'capture restarting', [window], 60)['passed'])
        self.assertFalse(fault_outcome(good, 7, 0, '', [window], 60)['passed'])
        for change in (dict(fresh_fps=0), dict(picture_coverage=.9), dict(seconds=1),
                       dict(elapsed_seconds=3), dict(elapsed_seconds=28), dict(seconds=26)):
            with self.subTest(change=change):
                self.assertFalse(fault_outcome(good, 7, 0, 'capture restarting', [window | change], 60)['passed'])
        self.assertTrue(fault_outcome(before, 7, 1, 'session failed\nTERMINATED error=-1', [], 60)['passed'])
        self.assertFalse(fault_outcome(before, 7, 1, 'session failed\nTERMINATED error=0', [], 60)['passed'])

    def test_rc23_recovery_warning_and_next_video_window_pass(self):
        samples = [dict(elapsed_seconds=3.8371067, sessions=[dict(frames_sent=201, warnings=[])]),
                   dict(elapsed_seconds=12.8325853, sessions=[dict(frames_sent=726, warnings=[])]),
                   dict(elapsed_seconds=16.6674114, sessions=[dict(frames_sent=956, warnings=[])])]
        windows = [dict(elapsed_seconds=3.002728, seconds=9.9840329, frames=584,
                        fresh_fps=58.1929172, picture_coverage=1.0, arrival_stutters=1, idr_frames=1),
                   dict(elapsed_seconds=13.0033206, seconds=6.6929036, frames=406,
                        fresh_fps=60.5118532, picture_coverage=1.0, arrival_stutters=0, idr_frames=0)]
        for code in ('capture_recovery', 'encoder_recovery', 'encoder_compute_recovery'):
            with self.subTest(code=code):
                log = f'2026-10-08T12:04:34.039415Z  WARN butterpollo_core::session: Capture interrupted; reopening capture. code="{code}"'
                result = fault_outcome(samples, 8.5164757, 0, log, windows, 60)
                self.assertTrue(result['passed'])
                self.assertEqual(result['outcome'], 'recovered')
                self.assertAlmostEqual(result['recovery_observed_seconds'], 11.1797485)
                self.assertFalse(fault_outcome(samples, 8.5164757, 0, log, windows[:1], 60)['passed'])
                self.assertFalse(fault_outcome(samples, 8.5164757, 0, log, windows, 120)['passed'])
        self.assertFalse(fault_outcome(samples, 8.5164757, 0, 'code="audio_stopped"', windows, 60)['passed'])
        self.assertFalse(fault_outcome([], 8.5164757, 0, 'code="capture_recovery"', windows, 60)['passed'])


if __name__ == '__main__':
    unittest.main()
