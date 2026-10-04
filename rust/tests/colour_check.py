"""Colour accuracy of a decoded HDR stream frame from motion_probe.

The probe draws a deterministic scRGB picture (examples/motion_probe.rs)
full-screen on the captured display. From the frame number in its barcode,
this recomputes every pixel's scRGB value, converts it as BT.2100 PQ with
BT.2020 primaries in the range the stream signals, and compares that with
the client's decoded Y'CbCr. moonlight_client.c writes FRAME.bin when
BUTTERPOLLO_TEST_FRAME_DUMP names a file (frame BUTTERPOLLO_TEST_FRAME_DUMP_AT,
600 by default). usage: colour_check.py FRAME.bin [...]"""
import json, pathlib, sys
import numpy as np

M709_2020 = np.array([[0.627404, 0.329283, 0.043313],
                      [0.069097, 0.919540, 0.011362],
                      [0.016391, 0.088013, 0.895595]])
KR, KB = 0.2627, 0.0593


def pq(nits):
    l = np.clip(nits / 10000.0, 0, 1)
    p = np.power(l, 2610 / 16384)
    return np.power((3424 / 4096 + 2413 / 128 * p) / (1 + 2392 / 128 * p), 2523 / 32)


def load(path):
    data = pathlib.Path(path).read_bytes()
    header, rest = data.split(b'\n', 1)
    _, w, h, fmt, depth, colour_range, colourspace, _ = header.decode().split()
    w, h = int(w), int(h)
    assert fmt == 'yuv420p10le', fmt
    planes = np.frombuffer(rest, dtype='<u2')
    y = planes[:w * h].reshape(h, w).astype(float)
    u = planes[w * h:w * h + (w // 2) * (h // 2)].reshape(h // 2, w // 2).astype(float)
    v = planes[w * h + (w // 2) * (h // 2):w * h + 2 * (w // 2) * (h // 2)].reshape(h // 2, w // 2).astype(float)
    return y, u, v, int(colour_range)


def word(y, row):
    centre = 8 + row * 24 + 12
    bits = [y[centre, 64 + 16 * i + 8] > 286 for i in range(32)]
    return sum(1 << i for i, bit in enumerate(bits) if bit)


def expected_rgb(frame, w, h):
    px, py = np.meshgrid(np.arange(w) + 0.5, np.arange(h) + 0.5)
    ax, ay = px + frame * 4, py + frame * 2
    grid = np.mod(np.floor(ax / 32) + np.floor(ay / 32), 2)[..., None]
    colour = (1 - grid) * np.array([0.02, 0.06, 0.12]) + grid * np.array([0.4, 0.22, 0.08])
    bar = np.mod(ax, 512)
    colour = colour + (bar / 1024)[..., None]
    colour = np.where((bar < 8)[..., None], np.array([4.0, 2.0, 1.0]), colour)
    band = (py >= 8) & (py < 104)
    colour = np.where((band & (px < 24))[..., None], 0.0, colour)
    colour = np.where((band & (px >= 24) & (px < 48))[..., None], 1.25, colour)
    return colour, band


def analyse(path):
    y, u, v, colour_range = load(path)
    h, w = y.shape
    if word(y, 3) != 0xB17E2212:
        return {'frame': str(path), 'error': 'no probe signature'}
    frame = word(y, 0)
    rgb, band = expected_rgb(frame, w, h)
    # BT.2100 PQ: scRGB 1.0 is 80 nits in BT.709 primaries.
    nonlinear = pq((rgb @ M709_2020.T) * 80)
    ey = nonlinear @ np.array([KR, 1 - KR - KB, KB])
    full = colour_range == 2
    y_code = 1023 * ey if full else 64 + 876 * ey
    # Chroma from the 2x2 average of nonlinear R'G'B'.
    blocks = nonlinear.reshape(h // 2, 2, w // 2, 2, 3).mean(axis=(1, 3))
    by = blocks @ np.array([KR, 1 - KR - KB, KB])
    cb = (blocks[..., 2] - by) / (2 * (1 - KB))
    cr = (blocks[..., 0] - by) / (2 * (1 - KR))
    scale = 1023 if full else 896
    u_code, v_code = 512 + scale * cb, 512 + scale * cr
    # Compare the picture outside the barcode band; patches separately.
    picture = ~band
    dy = (y - y_code)[picture]
    slope = np.polyfit(y_code[picture], y[picture], 1)[0]
    flat = (nonlinear.reshape(h // 2, 2, w // 2, 2, 3).std(axis=(1, 3)).max(axis=-1) < 1e-6)
    flat &= ~band[::2, ::2]
    expected_chroma = np.hypot(u_code - 512, v_code - 512)[flat]
    decoded_chroma = np.hypot(u - 512, v - 512)[flat]
    rows = slice(16, 96)
    return {
        'frame': pathlib.Path(path).parent.name, 'probe_frame': frame, 'range': 'full' if full else 'limited',
        'black_patch_y': round(float(y[rows, 4:20].mean()), 2), 'black_expected': round(float(y_code[rows, 4:20].mean()), 2),
        'white_patch_y': round(float(y[rows, 28:44].mean()), 2), 'white_expected': round(float(y_code[rows, 28:44].mean()), 2),
        'luma_mean_error': round(float(dy.mean()), 3), 'luma_mean_abs_error': round(float(np.abs(dy).mean()), 3),
        'luma_p99_abs_error': round(float(np.percentile(np.abs(dy), 99)), 2), 'contrast_slope': round(float(slope), 4),
        'saturation_ratio': round(float(decoded_chroma.sum() / expected_chroma.sum()), 4),
        'chroma_mean_abs_error': round(float(np.abs(np.stack([u, v])[:, flat] - np.stack([u_code, v_code])[:, flat]).mean()), 3),
    }


if __name__ == '__main__':
    for path in sys.argv[1:]:
        print(json.dumps(analyse(path)))
