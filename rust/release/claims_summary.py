"""Summarize a host log's per-claim pacing trace (RUST_LOG=info,pacing=trace).

usage: claims_summary.py butterpollo.log

Prints how many claims encoded a new picture and how many encoded an
unchanged one again (a static repeat or a recovery keyframe), with the time
from the picture's presentation to each claim and the gaps between claims.
"""
import re, sys


def quantiles(values):
    if not values:
        return 'none'
    values = sorted(values)
    pick = lambda p: values[min(len(values) - 1, int((len(values) - 1) * p))]
    return (f'n={len(values)} mean={sum(values) / len(values):.2f} p50={pick(.5):.2f} '
            f'p95={pick(.95):.2f} p99={pick(.99):.2f} max={values[-1]:.2f} ms')


claims = []
for line in open(sys.argv[1], encoding='utf-8', errors='replace'):
    if 'pacing' not in line or ' claim' not in line:
        continue
    fields = dict(re.findall(r'(\w+)=(\S+)', line))
    if 'claim' not in fields or 'fresh' not in fields:
        continue
    claims.append((int(fields['claim']), int(fields['presented']), fields['fresh'] == 'true'))
if not claims:
    sys.exit('no pacing claim trace in this log; run the host with RUST_LOG=info,pacing=trace')
fresh = [(claim - presented) / 1000 for claim, presented, new in claims if new]
again = [(claim - presented) / 1000 for claim, presented, new in claims if not new]
gaps = [(b[0] - a[0]) / 1000 for a, b in zip(claims, claims[1:])]
print(f'claims={len(claims)} new_pictures={len(fresh)} unchanged_again={len(again)}')
print('new picture, presented to claim:', quantiles(fresh))
print('unchanged again, presented to claim:', quantiles(again))
print('claim to claim:', quantiles(gaps))
