#!/usr/bin/env python3
"""Render Butterpollo's silent, measured-results launch demo.

Requires Python 3, Pillow and FFmpeg (libx264). No game, service or GPU access.
Usage: python3 docs/media/render_demo.py --output docs/media/demo.mp4
       python3 docs/media/render_demo.py --storyboard /tmp/storyboard.jpg
       python3 docs/media/render_demo.py --gif-from docs/media/demo.mp4 --output docs/media/demo.gif
       python3 docs/media/render_demo.py --diagram --output docs/media/compute-comparison.png

Fonts: Segoe UI on Windows/WSL, otherwise DejaVu Sans. Fonts are not bundled.
The historical comparison and latest validation deliberately have separate
on-screen version/setup labels. Detailed evidence lives in rust/PERFORMANCE.md.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from functools import lru_cache

from PIL import Image, ImageDraw, ImageFont

W, H = 1920, 1080
FPS = 30
DURATION = 54
BG = (13, 16, 22)
WHITE = (248, 249, 252)
YELLOW = (255, 210, 66)
MUTED = (164, 173, 191)
PANEL = (24, 29, 39)
LINE = (48, 56, 70)
SCENES = ((0, 10), (10, 16), (16, 25), (25, 34), (34, 41), (41, 49), (49, 54))


def font_path(bold=False, mono=False):
    name = 'consola.ttf' if mono else ('segoeuib.ttf' if bold else 'segoeui.ttf')
    roots = [Path(os.environ.get('WINDIR', 'C:/Windows')) / 'Fonts',
             Path('/mnt/c/Windows/Fonts')]
    for root in roots:
        if (root / name).is_file():
            return str(root / name)
    name = 'DejaVuSansMono.ttf' if mono else ('DejaVuSans-Bold.ttf' if bold else 'DejaVuSans.ttf')
    fallback = Path('/usr/share/fonts/truetype/dejavu') / name
    if fallback.is_file():
        return str(fallback)
    raise RuntimeError('Install Segoe UI or DejaVu Sans to render the demo.')


@lru_cache(maxsize=128)
def font(size, bold=False, mono=False):
    return ImageFont.truetype(font_path(bold, mono), size)


def text(d, xy, value, size=36, color=WHITE, bold=False, mono=False, anchor=None):
    d.text(xy, value, font=font(size, bold, mono), fill=color,
           anchor=anchor, stroke_width=0)


def ease(x):
    x = max(0, min(1, x))
    return 1 - (1 - x) ** 3


@lru_cache(maxsize=1)
def backdrop():
    im = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(im)
    # An understated warm-to-cool background keeps the bright proof cards clear.
    for y in range(H):
        p = y / H
        d.line((0, y, W, y), fill=(int(13 + 6 * p), int(16 + 7 * p), int(22 + 9 * p)))
    d.line((80, 150, 1840, 150), fill=LINE, width=2)
    return im


def brand(d, tag='RADEON STREAMING / TECHNICAL WALKTHROUGH'):
    d.rounded_rectangle((80, 63, 141, 124), radius=17, fill=YELLOW)
    text(d, (111, 92), 'B', 43, BG, True, anchor='mm')
    text(d, (160, 64), 'Butterpollo', 43, bold=True)
    text(d, (1838, 91), tag, 26, MUTED, mono=True, anchor='rm')


def label(d, value, y=190):
    text(d, (80, y), value, 29, YELLOW, bold=True)


def pill(d, x, y, value, fill=PANEL, fg=WHITE, size=27):
    box = d.textbbox((0, 0), value, font=font(size, True))
    width = box[2] - box[0] + 46
    height = max(58, size + 36)
    d.rounded_rectangle((x, y, x + width, y + height), radius=height // 2, fill=fill)
    text(d, (x + 23, y + height / 2 - 2), value, size, fg, True, anchor='lm')
    return width


def footer(d, lines):
    d.line((80, 948, 1840, 948), fill=LINE, width=2)
    for i, line in enumerate(lines):
        text(d, (80, 970 + i * 37), line, 27, MUTED)


def arrow(d, x, y, color=YELLOW):
    d.line((x - 23, y, x + 23, y), fill=color, width=4)
    d.line((x + 9, y - 13, x + 23, y, x + 9, y + 13), fill=color, width=4)


def progress(d, time):
    width = 1760 / len(SCENES)
    for index, (start, end) in enumerate(SCENES):
        x = 80 + index * width
        d.rounded_rectangle((x, 1058, x + width - 10, 1063), radius=2, fill=LINE)
        p = max(0, min(1, (time - start) / (end - start)))
        if p:
            d.rounded_rectangle((x, 1058, x + (width - 10) * p, 1063), radius=2, fill=YELLOW)


def block(d, box, title, fill=PANEL, fg=WHITE, size=31, outline=None):
    d.rounded_rectangle(box, radius=12, fill=fill, outline=outline, width=2)
    x0, y0, x1, y1 = box
    text(d, ((x0 + x1) / 2, (y0 + y1) / 2 - 2), title, size, fg, True, anchor='mm')


def path_arrow(d, points, color=YELLOW, width=3):
    d.line(points, fill=color, width=width, joint='curve')
    x, y = points[-2:]
    d.polygon([(x, y), (x - 12, y - 7), (x - 12, y + 7)], fill=color)


def pulse(d, x0, x1, y, time, duration=4):
    if time is None:
        return
    x = x0 + ((time / duration) % 1) * (x1 - x0)
    d.ellipse((x - 6, y - 6, x + 6, y + 6), fill=WHITE)


def encoder(d, x, y):
    d.rounded_rectangle((x, y, x + 180, y + 71), radius=12, fill=(43, 68, 84))
    text(d, (x + 90, y + 23), 'AMF', 29, WHITE, True, anchor='mm')
    text(d, (x + 90, y + 52), 'Hardware encode', 20, WHITE, anchor='mm')


def scene_queues(d, time):
    label(d, '01 / QUEUE PLACEMENT')
    text(d, (76, 238), 'Different queues. Same Radeon.', 81, bold=True)
    text(d, (82, 349), 'Capture copies + colour conversion move onto compute.', 38, MUTED)

    d.rounded_rectangle((80, 418, 1840, 616), radius=18, fill=PANEL, outline=LINE, width=2)
    text(d, (110, 434), 'SUNSHINE-DERIVED D3D11 PATH', 27, WHITE, True)
    text(d, (1812, 454), 'Reviewed: Vibepollo 2.0', 25, MUTED, anchor='rm')
    text(d, (111, 536), 'GRAPHICS', 26, MUTED, mono=True, anchor='lm')
    for i in range(3):
        x = 348 + i * 204
        block(d, (x, 501, x + 188, 572), 'Game', (54, 64, 81), size=29)
    block(d, (986, 501, 1222, 572), 'Capture copy', (136, 124, 82), size=28)
    block(d, (1240, 501, 1492, 572), 'RGB → YUV', (136, 124, 82), size=30)
    path_arrow(d, (1505, 536, 1615, 536), (148, 178, 205))
    encoder(d, 1630, 501)
    pulse(d, 1507, 1600, 536, time)

    d.rounded_rectangle((80, 646, 1840, 923), radius=18, fill=PANEL, outline=LINE, width=2)
    text(d, (110, 662), 'BUTTERPOLLO · D3D12 COMPUTE', 27, YELLOW, True)
    text(d, (1812, 682), 'Native AMF on both paths', 25, MUTED, anchor='rm')
    text(d, (111, 752), 'GRAPHICS', 26, MUTED, mono=True, anchor='lm')
    for i in range(3):
        x = 348 + i * 204
        block(d, (x, 717, x + 188, 788), 'Game', (54, 64, 81), size=29)
    text(d, (111, 857), 'COMPUTE', 26, YELLOW, mono=True, anchor='lm')
    block(d, (348, 822, 630, 893), 'Capture copies', YELLOW, BG, 29)
    block(d, (650, 822, 960, 893), 'RGB → NV12/P010', YELLOW, BG, 29)
    path_arrow(d, (978, 857, 1615, 857), YELLOW)
    text(d, (1278, 818), 'D3D12 surface + readiness fence', 29, MUTED, anchor='mm')
    encoder(d, 1630, 822)
    pulse(d, 980, 1600, 857, time)
    footer(d, ['Queue-placement schematic · both paths share one GPU and use GPU textures + native AMF',
               'Compute submission lets frame preparation run alongside graphics work.'])


def scene_fences(d, time):
    label(d, '02 / THE GPU HANDOFF')
    text(d, (76, 244), 'Explicit handoffs. Frame by frame.', 80, bold=True)
    text(d, (82, 359), 'GPU fences keep every producer, copy and encoder read in order.', 37, MUTED)
    titles = [('Captured', 'texture'), ('Fenced GPU', 'copies'), ('RGB →', 'NV12/P010'), ('Native', 'AMD AMF')]
    notes = [('Windows / WGC or DDX', 'Wait for producer readiness'),
             ('D3D12 compute queues', 'Copy-complete fence'),
             ('D3D12 compute shader', 'Per-texture ready fence'),
             ('D3D12 input surface', 'Texture ownership release')]
    for i, ((a, b), (n1, n2)) in enumerate(zip(titles, notes)):
        x = 80 + i * 465
        fill = YELLOW if i in (1, 2) else PANEL
        fg = BG if i in (1, 2) else WHITE
        d.rounded_rectangle((x, 468, x + 365, 670), radius=17, fill=fill)
        text(d, (x + 24, 489), f'0{i + 1}', 24, fg, mono=True)
        text(d, (x + 25, 534), a, 43, fg, True)
        text(d, (x + 25, 587), b, 43, fg, True)
        text(d, (x + 4, 712), n1, 28, WHITE)
        text(d, (x + 4, 757), n2, 27, MUTED)
        if i < 3:
            path_arrow(d, (x + 383, 567, x + 442, 567))
            pulse(d, x + 380, x + 436, 567, time, 3)
    d.line((1630, 811, 1630, 869, 995, 869), fill=LINE, width=3)
    d.polygon([(995, 869), (1009, 862), (1009, 876)], fill=LINE)
    text(d, (1290, 827), 'Reuse after the encoder releases ownership', 28, MUTED, anchor='mm')
    footer(d, ['Service WGC: capture copies include helper transfer + the host-owned snapshot.',
               'A separate fence for each output texture preserves frame readiness and safe reuse.'])


def pair_chart(d, x, y, title, values, max_value, time):
    text(d, (x, y), title, 36, WHITE, True)
    for i, (value, caption) in enumerate(zip(values, ('D3D11 graphics', 'D3D12 compute'))):
        yy = y + 81 + i * 142
        text(d, (x, yy), caption, 27, MUTED)
        text(d, (x + 765, yy + 2), f'{value:.1f} ms', 37, YELLOW if i else WHITE, True, anchor='rt')
        d.rounded_rectangle((x, yy + 58, x + 765, yy + 88), radius=5, fill=PANEL)
        length = 765 * value / max_value * ease(time / 1.25)
        d.rounded_rectangle((x, yy + 58, x + max(10, length), yy + 88), radius=5,
                            fill=YELLOW if i else (125, 137, 157))


def scene_compute_results(d, time):
    label(d, '03 / ISOLATE THE COMPUTE CHANGE')
    text(d, (76, 243), 'Same build. Compute off → on.', 85, bold=True)
    text(d, (82, 362), 'Full encrypted stream · test-pattern render → independent decoder', 36, MUTED)
    pair_chart(d, 84, 456, 'Average picture delay', (41.0, 33.5), 60, time)
    pair_chart(d, 1050, 456, '95th-percentile picture delay', (54.4, 42.3), 60, time)
    d.line((970, 459, 970, 813), fill=LINE, width=2)
    text(d, (83, 864), 'Separate encoder probe: 21.5 → 2.0 ms', 35, YELLOW, True)
    text(d, (900, 870), 'Synthetic frame submission → completed bitstream', 28, MUTED)
    footer(d, ['RX 7900 XT · 1080p60 HEVC 10-bit HDR · DDX · controlled GPU load · two runs per mode',
               'October 4, 2026 · gpu_compute_conversion = false / true · rust/PERFORMANCE.md'])


def scene_host_results(d, time):
    label(d, '04 / COMPARE THE COMPLETE HOSTS')
    text(d, (76, 243), 'Measure the picture that arrives.', 83, bold=True)
    text(d, (83, 359), 'Separate controlled comparison · three alternating runs per host', 37, MUTED)
    d.line((80, 460, 1840, 460), fill=LINE, width=2)
    text(d, (1120, 421), 'Vibepollo 2.0', 35, WHITE, True, anchor='mm')
    text(d, (1580, 421), 'Butterpollo rc.2', 35, YELLOW, True, anchor='mm')
    rows = [('Render → decode · average', '96.4 ms', '42.4 ms'),
            ('Render → decode · 95th percentile', '137.0 ms', '56.5 ms'),
            ('Fresh pictures per second', '23.9 FPS', '51.4 FPS')]
    for i, (name, old, new) in enumerate(rows):
        y = 524 + i * 130

        text(d, (83, y), name, 35, WHITE, anchor='lm')
        text(d, (1120, y), old, 53, (171, 181, 197), True, anchor='mm')
        text(d, (1580, y), new, 53, YELLOW, True, anchor='mm')
        if i < 2:
            d.line((80, y + 65, 1840, y + 65), fill=LINE, width=1)
    text(d, (83, 878), '56% lower average picture delay', 36, YELLOW, True)
    text(d, (1120, 878), '2.15× as many fresh pictures', 36, YELLOW, True)
    footer(d, ['RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps · matched AMF settings · GPU under load',
               'Butterpollo rc.2 vs Vibepollo 2.0 · October 4, 2026 · rust/PERFORMANCE.md'])


def scene_hdr(d, time):
    label(d, '05 / CURRENT rc.10 · NATIVE HDR VALIDATION')
    text(d, (76, 243), 'HDR, checked at the decoded pixels.', 77, bold=True)
    stages = [('FP16 scRGB', 'Native HDR capture'), ('PQ · BT.2020', 'GPU colour conversion'),
              ('10-bit HEVC / AV1', 'Native AMF encoding'), ('Decoded pixels', 'Independent verification')]
    for i, (title, detail) in enumerate(stages):
        x = 80 + i * 465
        d.rounded_rectangle((x, 407, x + 365, 578), radius=16, fill=PANEL, outline=LINE, width=2)
        text(d, (x + 22, 433), title, 33, YELLOW if i == 1 else WHITE, True)
        text(d, (x + 22, 516), detail, 27, MUTED)
        if i < 3:
            path_arrow(d, (x + 380, 491, x + 443, 491))
    text(d, (84, 639), '5,173 / 5,173', 83, YELLOW, True)
    text(d, (88, 756), 'HDR frames decoded', 36, WHITE)
    text(d, (1060, 639), '<0.51', 83, YELLOW, True)
    text(d, (1064, 756), 'Mean absolute colour error', 36, WHITE)
    text(d, (1064, 810), '10-bit code values · four reference frames', 27, MUTED)
    footer(d, ['rc.10 · RX 7900 XT · 1280×720/60 · native virtual HDR · FP16 capture · four HEVC/AV1 runs',
               '5,173 HDR + 1,169 SDR = 6,342 cleanly decoded frames · rust/PERFORMANCE.md'])


def sampling_grid(d, x, y, full, plane):
    side = 142
    base = (198, 204, 214) if plane == 'Y' else ((66, 175, 210) if plane == 'Cb' else (202, 108, 166))
    if full or plane == 'Y':
        for row in range(2):
            for col in range(2):
                delta = (row * 2 + col) * 10
                color = tuple(max(0, c - delta) for c in base)
                xx, yy = x + col * 75, y + row * 75
                d.rounded_rectangle((xx, yy, xx + 66, yy + 66), radius=8, fill=color)
                text(d, (xx + 33, yy + 33), '•', 28, BG, anchor='mm')
    else:
        d.rounded_rectangle((x, y, x + side, y + side), radius=8, fill=base)
        text(d, (x + side / 2, y + side / 2), '•', 28, BG, anchor='mm')
    text(d, (x + side / 2, y - 35), plane, 31, WHITE, True, anchor='mm')


def scene_pyrowave(d, time):
    label(d, '06 / PYROWAVE · FULL HDR 4:4:4')
    text(d, (76, 243), 'Full-resolution colour, pixel by pixel.', 76, bold=True)
    text(d, (83, 353), 'Chroma sampling in a 2×2-pixel block', 37, MUTED)
    for i, (mode, detail) in enumerate([('4:2:0', '4 Y + 1 Cb + 1 Cr sample'), ('4:4:4', '4 Y + 4 Cb + 4 Cr samples')]):
        x = 80 + i * 940
        d.rounded_rectangle((x, 425, x + 820, 794), radius=18, fill=PANEL, outline=LINE, width=2)
        text(d, (x + 31, 448), mode, 49, YELLOW if i else WHITE, True)
        text(d, (x + 785, 473), 'PyroWave HDR' if i else 'Subsampled chroma', 29, MUTED, anchor='rm')
        for j, plane in enumerate(('Y', 'Cb', 'Cr')):
            sampling_grid(d, x + 111 + j * 225, 575, bool(i), plane)
        text(d, (x + 410, 757), detail, 30, WHITE, anchor='mm')
    text(d, (83, 846), '10-bit planar YUV → shared D3D11 / Vulkan textures → PyroWave', 36, YELLOW, True)
    footer(d, ['Pair with Nonary’s Moonlight client on a fast wired LAN.',
               'Recorded 1080p/120 HDR 4:4:4 stream: all 2,357 received frames decoded.'])


def scene_cta(d, time):
    label(d, 'BUTTERPOLLO rc.10')
    text(d, (76, 249), 'Built for Radeon. Built on great work.', 76, bold=True)
    text(d, (83, 383), 'WGC + compute by default   ·   PyroWave HDR 4:4:4', 40, YELLOW, True)
    text(d, (83, 448), 'Virtual displays   ·   RTSS   ·   Steam + Playnite   ·   263 tests passed', 34, MUTED)
    text(d, (82, 568), 'Thanks, Nonary.', 76, WHITE, True)
    text(d, (86, 671), 'For Vibepollo and the Moonlight client work.', 38, WHITE)
    text(d, (86, 731), 'With credit to Sunshine, Apollo, Themaister and joemossjr16.', 31, MUTED)
    text(d, (83, 849), 'github.com/RamazanKara/Butterpollo', 46, YELLOW, True)
    footer(d, ['Install → Pair Moonlight → Play',
               'Open source · written in Rust · full measurements and source linked in the README'])


SCENE_DRAWERS = [scene_queues, scene_fences, scene_compute_results, scene_host_results,
                 scene_hdr, scene_pyrowave, scene_cta]

def render_scene(index, time):
    im = backdrop().copy()
    d = ImageDraw.Draw(im)
    brand(d)
    SCENE_DRAWERS[index](d, time)
    return im


def frame(time):
    index = next((i for i, (_, end) in enumerate(SCENES) if time < end), len(SCENES) - 1)
    local = time - SCENES[index][0]
    im = render_scene(index, local)
    if index and local < .22:
        previous = render_scene(index - 1, SCENES[index - 1][1] - SCENES[index - 1][0])
        im = Image.blend(previous, im, ease(local / .22))
    progress(ImageDraw.Draw(im), time)
    return im


def encode_gif(source, destination):
    ffmpeg = shutil.which('ffmpeg')
    if not ffmpeg:
        raise RuntimeError('FFmpeg is required to encode the preview.')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='butterpollo-demo-') as directory:
        palette = str(Path(directory) / 'palette.png')
        base = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', '-threads', '4']
        subprocess.run(base + ['-i', str(source), '-vf',
                       'fps=12,scale=960:540:flags=lanczos,palettegen=max_colors=128:stats_mode=diff',
                       '-frames:v', '1', palette], check=True)
        subprocess.run(base + ['-i', str(source), '-i', palette, '-filter_complex',
                       '[0:v]fps=12,scale=960:540:flags=lanczos[x];'
                       '[x][1:v]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle',
                       '-loop', '0', str(destination)], check=True)
    print(f'Wrote {destination}', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--storyboard', type=Path)
    parser.add_argument('--frame', type=float)
    parser.add_argument('--gif-from', type=Path)
    parser.add_argument('--diagram', action='store_true')
    args = parser.parse_args()
    if args.diagram:
        if not args.output:
            parser.error('--diagram requires --output.')
        args.output.parent.mkdir(parents=True, exist_ok=True)
        render_scene(0, None).save(args.output)
        return
    if args.gif_from:
        if not args.output:
            parser.error('--gif-from requires --output.')
        encode_gif(args.gif_from, args.output)
        return
    if args.storyboard:
        sheet = Image.new('RGB', (960 * 2, 540 * 4), BG)
        for i, (start, end) in enumerate(SCENES):
            still = frame((start + end) / 2).resize((960, 540), Image.Resampling.LANCZOS)
            sheet.paste(still, ((i % 2) * 960, (i // 2) * 540))
        args.storyboard.parent.mkdir(parents=True, exist_ok=True)
        sheet.save(args.storyboard)
    if args.output and args.frame is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        frame(args.frame).save(args.output)
    elif args.output:
        ffmpeg = shutil.which('ffmpeg')
        if not ffmpeg:
            raise RuntimeError('FFmpeg is required to encode the demo.')
        args.output.parent.mkdir(parents=True, exist_ok=True)
        cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y',
               '-f', 'rawvideo', '-pixel_format', 'rgb24', '-video_size', f'{W}x{H}',
               '-framerate', str(FPS), '-i', 'pipe:0', '-an',
               '-vf', 'scale=out_color_matrix=bt709:out_range=tv', '-c:v', 'libx264',
               '-threads', '4', '-preset', 'medium', '-crf', '20', '-pix_fmt', 'yuv420p',
               '-movflags', '+faststart', '-color_primaries', 'bt709',
               '-color_trc', 'bt709', '-colorspace', 'bt709', '-color_range', 'tv', str(args.output)]
        with subprocess.Popen(cmd, stdin=subprocess.PIPE) as process:
            try:
                for n in range(DURATION * FPS):
                    process.stdin.write(frame(n / FPS).tobytes())
                    if n % (FPS * 6) == 0:
                        print(f'Rendered {n // FPS}/{DURATION} seconds', flush=True)
            finally:
                process.stdin.close()
            code = process.wait()
            if code:
                raise RuntimeError(f'FFmpeg failed with exit code {code}')
        print(f'Wrote {args.output}', flush=True)
    if not args.output and not args.storyboard:
        parser.error('Choose --output or --storyboard.')


if __name__ == '__main__':
    main()
