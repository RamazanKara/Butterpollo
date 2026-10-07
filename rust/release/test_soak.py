import csv, pathlib, tempfile, unittest
from soak_result import audio_windows, fault_outcome, resource_growth, video_windows
from soak import cases


class SoakMeasurements(unittest.TestCase):
    def test_long_cases_retain_all_codecs_and_alternate_load(self):
        streams = cases('long')[:6]
        self.assertEqual([s['load'] for s in streams], [False, True, True, False, False, True])
        self.assertTrue(all(1200 <= s['seconds'] <= 1800 for s in streams))
        self.assertEqual(sum(s.get('churn', False) for s in cases('short')), 52)
        self.assertTrue(any(s.get('resume') for s in cases('short')))

    def test_windows_detect_a_late_freeze_beyond_the_old_receiver_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'frames.csv'
            with path.open('w', newline='') as stream:
                writer = csv.writer(stream)
                writer.writerow(['arrival_ms', 'assembled_us', 'picture_age_ms', 'render_frame', 'frame_type'])
                for i in range(108001):
                    writer.writerow([i * 1000 / 60, i * 1e6 / 60, 12 if i < 107000 else 200,
                                     min(i + 1, 107000), 1 if i == 0 else 2])
            windows = video_windows(path, 60)
            self.assertGreater(windows[-1]['elapsed_seconds'], 1700)
            self.assertLess(windows[-1]['fresh_fps'], 1)
            self.assertEqual(windows[-1]['picture_age_p99_ms'], 200)

    def test_two_period_arrival_gaps_are_counted_without_calling_them_send_gaps(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'frames.csv'
            path.write_text('arrival_ms,assembled_us,picture_age_ms,render_frame,frame_type\n'
                            '0,0,10,1,1\n20,20000,12,2,2\n60,60000,14,3,2\n')
            window = video_windows(path, 60, 0)[0]
            self.assertEqual(window['arrival_stutters'], 1)
            self.assertEqual(window['idr_frames'], 1)

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

    def test_fault_requires_sustained_progress_or_explicit_error(self):
        def sample(seconds, frames):
            return dict(elapsed_seconds=seconds, sessions=[dict(frames_sent=frames)])
        before = [sample(5, 300)]
        frozen = before + [sample(12, 360), sample(17, 360)]
        self.assertFalse(fault_outcome(frozen, 7, 0, 'capture restarting')['passed'])
        good = before + [sample(12, 600), sample(17, 900)]
        self.assertTrue(fault_outcome(good, 7, 0, 'capture restarting')['passed'])
        self.assertFalse(fault_outcome(good, 7, 0, '')['passed'])
        self.assertTrue(fault_outcome(before, 7, 1, 'session failed\nTERMINATED error=-1')['passed'])
        self.assertFalse(fault_outcome(before, 7, 1, 'session failed\nTERMINATED error=0')['passed'])


if __name__ == '__main__':
    unittest.main()
