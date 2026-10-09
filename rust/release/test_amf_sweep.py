import json, pathlib, shutil, subprocess, tempfile, unittest
from unittest.mock import patch
from amf_sweep import FRAMES, estimate, fixture_source, quartet, settings
from amf_sweep_result import ARMS, age_rows, picture_metrics, quality_rows, read_runs, score_metrics, select_candidates, summarize


def quality(candidate='amd_vbaq=disabled', rate=80000, score=.98, codec='hevc', profile='native120'):
    rows = []
    for arm in ARMS:
        control = arm.startswith('A')
        rows.append(dict(kind='quality', comparison=f'{profile}-{codec}-{candidate}', profile=profile, codec=codec,
            candidate=candidate, settings=dict([candidate.split('=')]), target_kbps=80000, fps=120,
            requested_kbps=80000, arm=arm, status='ok', actual_bitrate_kbps=80000 if control else rate,
            ssim_all=.97 if control else score, psnr_y=40 if control else 42, encode_mean_ms=3, encode_p95_ms=4,
            encode_p99_ms=5, frame_bytes_mean=81000, frame_bytes_p99=100000, all_frame_bytes_p99=120000,
            frame_bytes_max=200000, idr_bytes=[200000]))
    return rows


def receiver(**changes):
    fields = dict(mean=12, p95=15, p99=16, maximum=18, fps=120, coverage=1, decoded=1200, failures=0, decode_ms=3)
    fields.update(changes)
    return (f"RESULT frames=1200 decoded_frames={fields['decoded']} audio_packets=200 failures={fields['failures']}\n"
            f"VISUAL frames=1000 unique=1000 unique_fps={fields['fps']} coverage={fields['coverage']}\n"
            f"PICTURE_AGE samples=1000 mean_ms={fields['mean']} p50_ms=12 p95_ms={fields['p95']} p99_ms={fields['p99']} max_ms={fields['maximum']}\n"
            f"PERFORMANCE seconds=12 decoder_mean_ms={fields['decode_ms']}\n")


class AmfSweepMeasurements(unittest.TestCase):
    def test_equal_actual_rank_does_not_reward_extra_bits(self):
        rows = quality('amd_rc=cbr', rate=90000, score=.999) + quality()
        result = quality_rows(rows)
        matched = next(r for r in result if r['candidate'].startswith('amd_vbaq'))
        expensive = next(r for r in result if r['candidate'].startswith('amd_rc'))
        self.assertEqual(matched['quality_rank'], 1)
        self.assertEqual(expensive['requested_rank'], 1)
        self.assertIsNone(expensive['quality_rank'])
        self.assertFalse(expensive['equal_actual'])

    def test_second_b_uses_actual_rate_and_paired_default(self):
        rows = quality(rate=96000)
        rows[-1].update(requested_kbps=66000, actual_bitrate_kbps=79000)
        result = quality_rows(rows)[0]
        self.assertTrue(result['equal_actual'])
        self.assertEqual(result['requested_actual_kbps'], 96000)
        self.assertEqual(result['matched_request_kbps'], 66000)
        self.assertEqual(result['frame_bytes_max'], 200000)
        self.assertEqual(result['idr_bytes'], [200000])
        self.assertAlmostEqual(result['delta_ssim'], .01)

    def test_actual_bitrate_tolerance_includes_boundary(self):
        self.assertTrue(quality_rows(quality(rate=82400))[0]['equal_actual'])
        self.assertFalse(quality_rows(quality(rate=82401))[0]['equal_actual'])

    def test_runner_alternates_controls_and_calibrates_only_last_candidate(self):
        with patch('amf_sweep.quality') as run:
            run.side_effect = [dict(status='ok', actual_bitrate_kbps=64000), dict(status='ok', actual_bitrate_kbps=80000), {}, {}]
            quartet(None, None, ('native120', 1968, 2184, 120, 80000), 'hevc', 'amd_rc', 'cbr')
            self.assertEqual([c.args[-2:] for c in run.call_args_list], [('A1', 80000), ('B1', 80000), ('A2', 80000), ('B2', 64000)])

    def test_incomplete_failed_or_reordered_quartets_are_not_ranked(self):
        for kind in ('missing', 'failed', 'order', 'duplicate'):
            with self.subTest(kind=kind):
                rows = quality()
                if kind == 'missing': rows.pop()
                if kind == 'failed': rows[1]['status'] = 'failed'
                if kind == 'order': rows[1], rows[2] = rows[2], rows[1]
                if kind == 'duplicate': rows.append(rows[-1])
                result = quality_rows(rows)[0]
                self.assertFalse(result['complete'])
                self.assertIsNone(result['quality_rank'])

    def test_ranks_are_within_profile_codec_and_use_adjacent_control(self):
        rows = quality(score=.98) + quality('amd_quality=balanced', score=.99)
        rows[4]['ssim_all'] = rows[6]['ssim_all'] = .985
        rows += quality(codec='av1', profile='1080p60', score=.96)
        results = quality_rows(rows)
        self.assertEqual(next(r for r in results if r['codec'] == 'av1')['quality_rank'], 1)
        self.assertEqual(next(r for r in results if r['candidate'] == 'amd_vbaq=disabled' and r['codec'] == 'hevc')['quality_rank'], 1)

    def test_top_three_exclude_incomplete_and_over_budget(self):
        rows = []
        for index in range(5):
            group = quality(f'amd_max_frame_size={index}', score=.98 + index * .001)
            if index == 4: group[-1]['encode_p99_ms'] = 9
            rows += group
        rows += quality('amd_rc=cbr', rate=100000, score=.999)
        selected = select_candidates(rows, 'native120', 'hevc')
        self.assertEqual([r['candidate'] for r in selected], [f'amd_max_frame_size={i}' for i in (3, 2, 1)])
        self.assertTrue(all(r['selection'] == 'equal actual bitrate' for r in selected))

    def test_fallback_selection_never_calls_unmatched_rate_equal(self):
        result = select_candidates(quality(rate=100000), 'native120', 'hevc')[0]
        self.assertIn('unmatched', result['selection'])
        self.assertIsNone(result['quality_rank'])

    def test_receiver_rejects_missing_starved_failed_and_bad_age(self):
        self.assertTrue(picture_metrics(receiver(), 120, 0)['valid'])
        self.assertFalse(picture_metrics('', 120, 0)['valid'])
        self.assertFalse(picture_metrics(receiver(), 120, 1)['valid'])
        for change in (dict(fps=100), dict(coverage=.90), dict(decoded=1199), dict(failures=1),
                       dict(decode_ms=9), dict(p99=99), dict(mean=30)):
            with self.subTest(change=change):
                self.assertFalse(picture_metrics(receiver(**change), 120, 0)['valid'])

    def test_age_ranks_mean_of_paired_p99_and_requires_all_four_runs(self):
        rows = [row | dict(kind='age') | picture_metrics(receiver(p99=17 if row['arm'].startswith('A') else 16), 120, 0)
                for row in quality()]
        result = age_rows(rows)[0]
        self.assertTrue(result['valid'])
        self.assertEqual(result['delta_p99_ms'], -1)
        self.assertEqual(result['age_rank'], 1)
        rows[-1]['valid'] = False
        self.assertIsNone(age_rows(rows)[0]['age_rank'])
        self.assertIsNone(age_rows(rows[:-1])[0]['age_rank'])

    def test_metric_parser_validates_count_planes_and_missing_metrics(self):
        log = '[Parsed_psnr] PSNR y:41.2 u:42.3 v:43.4 average:42.1 min:30 max:inf\n' \
              '[Parsed_ssim] SSIM Y:0.98 (17) U:0.99 (20) V:0.99 (20) All:0.983 (18)\n'
        with tempfile.TemporaryDirectory() as temporary:
            psnr, ssim = pathlib.Path(temporary) / 'psnr', pathlib.Path(temporary) / 'ssim'
            for path in (psnr, ssim): path.write_text('n:1 value:1\nn:2 value:1\n')
            metrics = score_metrics(log, psnr, ssim, 2)
            self.assertEqual(metrics['psnr_v'], 43.4)
            self.assertEqual(metrics['ssim_all'], .983)
            self.assertEqual(score_metrics(log.replace('y:41.2', 'y:inf'), psnr, ssim, 2)['psnr_y'], float('inf'))
            for bad in ('', log.replace('All:0.983', 'All:nan'), log.replace('All:0.983', 'All:1.2')):
                with self.assertRaises(ValueError): score_metrics(bad, psnr, ssim, 2)
            ssim.write_text('n:1 value:1\n')
            with self.assertRaises(ValueError): score_metrics(log, psnr, ssim, 2)

    def test_summary_writes_partial_results_and_all_metrics(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            rows = quality() + quality('amd_rc=cbr')[:2]
            (root / 'runs.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in rows))
            (root / 'status.txt').write_text('ABORTED: synthetic WATCHDOG report')
            summarize(root)
            self.assertIn('ABORTED', (root / 'summary.md').read_text())
            self.assertIn('not native HDR', (root / 'summary.md').read_text())
            self.assertIn('actual_bitrate_kbps', (root / 'runs.csv').read_text())
            self.assertIn('requested_rank', (root / 'quality.csv').read_text())
            self.assertTrue((root / 'picture-age.csv').exists())

    def test_interrupted_json_append_preserves_completed_runs(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'runs.jsonl'
            path.write_text(json.dumps(quality()[0]) + '\n{"kind": "quality"')
            self.assertEqual(len(read_runs(path.parent)), 1)
            summarize(path.parent)
            self.assertIn('INCOMPLETE', path.with_name('summary.md').read_text())

    def test_all_ranked_settings_and_bounded_estimate(self):
        hevc, av1, h264 = (settings(c) for c in ('hevc', 'av1', 'h264'))
        self.assertEqual(sorted(set(r for r, _, _ in av1)), list(range(1, 9)))
        self.assertIn((6, 'QueryTimeout', '0'), hevc)
        self.assertIn((6, 'amd_input_queue_size', '1'), hevc)
        self.assertIn((2, 'amd_ltr_frames', '2'), h264)
        self.assertIn((5, 'amd_av1_tiles', '4'), av1)
        self.assertNotIn((8, 'amd_split_frame', 'enabled'), h264)
        self.assertLess(estimate()['estimate_minutes'], 90)
        self.assertEqual(estimate()['age_runs_max'], 108)
        self.assertEqual(FRAMES, 32)

    def test_existing_motion_fixture_replaces_overrides_without_changing_measurement(self):
        with tempfile.TemporaryDirectory() as temporary:
            bench = pathlib.Path(temporary)
            source = ("render('40')\naudio('40')\n"
                      "config = 'amd_rc = vbr_latency\\nvirtual_display_layout = exclusive\\n'\n"
                      "for line in ['amd_rc = cbr', 'virtual_display_layout = extended']:\n"
                      "    config += line.strip() + '\\n'\nfinally_cleanup()\n")
            (bench / 'run-motion.py').write_text(source)
            with patch('amf_sweep.BENCH', bench):
                path, adapted = fixture_source()
                self.assertEqual(path, bench / 'run-motion.py')
                calls = []
                fixture = dict(render=lambda v: calls.append(v), audio=lambda v: calls.append(v),
                               finally_cleanup=lambda: calls.append('cleanup'))
                exec(adapted, fixture)
                self.assertEqual(calls, ['16', '16', 'cleanup'])
                self.assertEqual(fixture['config'], 'amd_rc = cbr\nvirtual_display_layout = extended\n')
                self.assertEqual(path.read_text(), source)
                path.write_text(source + "unexpected('40')")
                with self.assertRaises(ValueError): fixture_source()

    @unittest.skipUnless(shutil.which('pwsh'), 'PowerShell is not installed')
    def test_safety_functions_fail_closed_with_mocked_host_gpu_and_windows(self):
        script = pathlib.Path(__file__).with_name('amf_sweep.ps1')
        with tempfile.TemporaryDirectory() as temporary:
            # Import only the function ASTs. Never execute the hardware runner or its real window enumerator.
            test = r'''
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile('SCRIPT', [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'PowerShell parse failure' }
foreach ($function in $ast.FindAll({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst]}, $true)) {
    . ([scriptblock]::Create($function.Extent.Text))
}
Add-Type 'public static class AmfSweepWindows { public static object[] Windows = new object[0]; public static object[] Find() { return Windows; } }'
$Work = 'TEMP'
$started = [Diagnostics.Stopwatch]::StartNew()
$estimate = @{ hard_stop_minutes = 88 }
$watchedLogs = @{}; $initialReports = @{}; $worker = $null
$script:reports = @()
$script:gpu = [pscustomobject]@{FriendlyName='Radeon'; Status='OK'; Problem='CM_PROB_NONE'; InstanceId='fixture'}
$script:xml = '<root><RustHostSessionCount>0</RustHostSessionCount><RustHostPendingSessionCount>0</RustHostPendingSessionCount></root>'
function Get-PnpDevice { param($Class, [switch]$PresentOnly) return $script:gpu }
function Get-Reports { return $script:reports }
function Invoke-WebRequest { param($Uri, [switch]$NoProxy, $TimeoutSec) return @{Content=$script:xml} }
function Expect-Stop([string] $Pattern) {
    $caught = $false
    try { Assert-Safe 'test' } catch { if ($_.Exception.Message -notmatch $Pattern) { throw }; $caught = $true }
    if (!$caught) { throw "guard accepted $Pattern" }
}
Assert-Safe 'idle'
$script:gpu.Status='Error'; Expect-Stop 'Radeon'; $script:gpu.Status='OK'
$script:gpu.Problem='22'; Expect-Stop 'Radeon'; $script:gpu.Problem='CM_PROB_NONE'
$script:reports=@(@{path='new.dmp'; signature='1'}); Expect-Stop 'WATCHDOG'
$initialReports['new.dmp']='1'; Assert-Safe 'old report'
$script:reports[0].signature='2'; Expect-Stop 'WATCHDOG'; $script:reports=@()
$script:xml='<root><RustHostSessionCount>0</RustHostSessionCount></root>'; Expect-Stop 'missing'
$script:xml='<root><RustHostSessionCount>0</RustHostSessionCount><RustHostPendingSessionCount>1</RustHostPendingSessionCount></root>'; Expect-Stop 'busy'
$script:xml='<root><RustHostSessionCount>0</RustHostSessionCount><RustHostPendingSessionCount>0</RustHostPendingSessionCount></root>'
[AmfSweepWindows]::Windows=@([pscustomobject]@{Pid=99;Title='game'}); Expect-Stop 'Fullscreen'
[AmfSweepWindows]::Windows=@()
function Invoke-WebRequest { throw 'unreachable installed host' }
Expect-Stop 'unreachable'
$log = Join-Path $Work 'gpu.log'
[IO.File]::WriteAllText($log, 'device rem')
$watchedLogs[$log]=0L
Test-Logs
[IO.File]::AppendAllText($log, 'oved')
try { Test-Logs; throw 'GPU error accepted' } catch { if ($_.Exception.Message -notmatch 'GPU/stall log evidence') { throw } }
'''.replace('SCRIPT', str(script).replace("'", "''")).replace('TEMP', temporary.replace("'", "''"))
            path = pathlib.Path(temporary) / 'guard-test.ps1'
            path.write_text(test, encoding='utf-8')
            result = subprocess.run(['pwsh', '-NoProfile', '-File', str(path)], capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
