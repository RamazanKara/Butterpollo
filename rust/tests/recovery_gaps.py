"""Picture stalls in a test client's timing CSV (BUTTERPOLLO_TEST_TIMING_CSV).

Each row is a frame the client submitted for decoding. Frames lost on the
network, and frames the client drops while it waits for a keyframe, leave a gap
in arrival time and in the wire frame number. With BUTTERPOLLO_TEST_SEND_OUTAGE
on the host, each outage is one stall; its length is outage plus recovery.
recovery_kb is the size of the frame that ends a stall (a keyframe, or with
reference invalidation a frame predicted from an older one), against the
median frame.

    python recovery_gaps.py timing.csv [more.csv ...]
"""

import csv
import statistics
import sys


def stalls(path):
    with open(path, newline="") as handle:
        rows = [
            (
                int(row["wire_frame"]),
                float(row["arrival_ms"]),
                int(row["frame_type"]),
                int(row.get("bytes") or 0),
            )
            for row in csv.DictReader(handle)
        ]
    if len(rows) < 3:
        raise SystemExit(f"{path}: too few frames")
    intervals = [b[1] - a[1] for a, b in zip(rows, rows[1:])]
    period = statistics.median(intervals)
    found = []
    for (frame, at, _, _), (next_frame, next_at, kind, size) in zip(rows, rows[1:]):
        if next_frame - frame > 1 and next_at - at > 2 * period:
            # frame_type 1 is Moonlight's FRAME_TYPE_IDR.
            found.append((next_at - at, next_frame - frame - 1, kind == 1, size))
    return len(rows), period, statistics.median(row[3] for row in rows), found


def percentile(values, share):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int((len(ordered) - 1) * share))]


def main(paths):
    every = []
    for path in paths:
        frames, period, median_bytes, found = stalls(path)
        every.extend(found)
        gaps = [stall[0] for stall in found]
        print(
            f"{path}: frames={frames} period_ms={period:.3f} stalls={len(found)}"
            f" frame_kb_median={median_bytes / 1024:.1f}"
            + (
                f" stall_mean_ms={statistics.fmean(gaps):.1f}"
                f" p95_ms={percentile(gaps, 0.95):.1f} max_ms={max(gaps):.1f}"
                f" frames_missing={sum(stall[1] for stall in found)}"
                f" ended_by_keyframe={sum(stall[2] for stall in found)}"
                f" recovery_kb_mean={statistics.fmean(stall[3] for stall in found) / 1024:.1f}"
                if found
                else ""
            )
        )
    if len(paths) > 1 and every:
        gaps = [stall[0] for stall in every]
        print(
            f"ALL stalls={len(every)} stall_mean_ms={statistics.fmean(gaps):.1f}"
            f" p50_ms={percentile(gaps, 0.5):.1f} p95_ms={percentile(gaps, 0.95):.1f}"
            f" max_ms={max(gaps):.1f} frames_missing_mean={statistics.fmean(stall[1] for stall in every):.2f}"
            f" ended_by_keyframe={sum(stall[2] for stall in every)}"
            f" recovery_kb_mean={statistics.fmean(stall[3] for stall in every) / 1024:.1f}"
        )


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    main(sys.argv[1:])
