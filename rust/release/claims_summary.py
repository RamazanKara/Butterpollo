"""Summarize a host log's per-claim pacing trace (RUST_LOG=info,pacing=trace).

usage: claims_summary.py butterpollo.log

Prints how many claims encoded a new picture and how many encoded an
unchanged one again (a static repeat or a recovery keyframe), with the time
from the picture's presentation to each claim and the gaps between claims.
"""
import sys
from e2e_result import claims


def quantiles(values):
    if not values:
        return 'none'
    values = sorted(values)
    pick = lambda p: values[min(len(values) - 1, int((len(values) - 1) * p))]
    return (f'n={len(values)} mean={sum(values) / len(values):.2f} p50={pick(.5):.2f} '
            f'p95={pick(.95):.2f} p99={pick(.99):.2f} max={values[-1]:.2f} ms')


found = claims(open(sys.argv[1], encoding='utf-8', errors='replace').read())
if not found:
    sys.exit('no pacing claim trace in this log; run the host with RUST_LOG=info,pacing=trace')
fresh = [(claim - presented) / 1000 for claim, presented, new in found if new]
again = [(claim - presented) / 1000 for claim, presented, new in found if not new]
gaps = [(b[0] - a[0]) / 1000 for a, b in zip(found, found[1:])]
print(f'claims={len(found)} new_pictures={len(fresh)} unchanged_again={len(again)}')
print('new picture, presented to claim:', quantiles(fresh))
print('unchanged again, presented to claim:', quantiles(again))
print('claim to claim:', quantiles(gaps))
