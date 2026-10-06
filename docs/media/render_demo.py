#!/usr/bin/env python3
"""Render Butterpollo's silent, measured-results launch demo.

Requires Python 3, Pillow and FFmpeg (libx264). No game, service or GPU access.
Usage: python3 docs/media/render_demo.py --output docs/media/demo.mp4
       python3 docs/media/render_demo.py --storyboard /tmp/storyboard.jpg
       python3 docs/media/render_demo.py --gif-from docs/media/demo.mp4 --output docs/media/demo.gif

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
DURATION = 42
BG = (13, 16, 22)
WHITE = (248, 249, 252)
YELLOW = (255, 210, 66)
MUTED = (164, 173, 191)
PANEL = (24, 29, 39)
LINE = (48, 56, 70)
SCENES = ((0, 4.5), (4.5, 9.5), (9.5, 15), (15, 20.5),
          (20.5, 26), (26, 32), (32, 37.5), (37.5, 42))


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
    for x in range(64, W, 64):
        for y in range(64, H, 64):
            d.ellipse((x, y, x + 2, y + 2), fill=(33, 39, 50))
    d.line((80, 150, 1840, 150), fill=LINE, width=2)
    return im


def brand(d, tag='BUILT FOR AMD. MADE FOR MOONLIGHT.'):
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


def scene_hero(d, time):
    label(d, 'BUTTERPOLLO rc.10 · BUILT FOR RADEON')
    text(d, (75, 274), 'Game hard.', 153, bold=True)
    text(d, (75, 437), 'Stream smooth.', 153, YELLOW, True)
    text(d, (83, 650), '56% lower picture delay. 2.15× as many fresh pictures.', 47)
    x = 82
    for item in ('WGC CAPTURE', 'RADEON COMPUTE', 'NATIVE AMF'):
        x += pill(d, x, 758, item) + 18
    # GPU tile: real architecture vocabulary, abstract visual rather than fake footage.
    x, y, s = 1512, 507, 228
    for k in range(8):
        p = (time * .2 + k / 8) % 1
        radius = 154 + p * 92
        fade = 1 - p
        c = tuple(int(a * fade + b * p) for a, b in zip((100, 87, 40), BG))
        d.rounded_rectangle((x - radius, y - radius, x + radius, y + radius), radius=48, outline=c, width=2)
    d.rounded_rectangle((x - s / 2, y - s / 2, x + s / 2, y + s / 2), radius=36, fill=YELLOW)
    text(d, (x, y - 31), 'AMD', 62, BG, True, anchor='mm')
    text(d, (x, y + 42), 'RADEON', 29, BG, True, anchor='mm')
    footer(d, ['Measured: RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps · controlled GPU load',
               'Butterpollo rc.2 vs Vibepollo 2.0 · three runs per host · October 4, 2026'])


def scene_compute(d, time):
    label(d, 'RADEON COMPUTE · ON BY DEFAULT')
    text(d, (76, 253), 'Your GPU. A smarter stream.', 108, bold=True)
    text(d, (82, 416), 'Capture copies + colour conversion on D3D12 compute queues.', 43, MUTED)
    cards = [(80, '01', 'WGC', 'Fast Windows capture'),
             (690, '02', 'COMPUTE', 'GPU copies + colour'),
             (1300, '03', 'AMF', 'Native AMD encoding')]
    for x, number, title, subtitle in cards:
        highlight = title == 'COMPUTE'
        d.rounded_rectangle((x, 555, x + 540, 797), radius=26,
                            fill=YELLOW if highlight else PANEL, outline=None if highlight else LINE, width=2)
        fg = BG if highlight else WHITE
        text(d, (x + 30, 581), number, 28, fg, mono=True)
        text(d, (x + 30, 621), title, 65, fg, True)
        text(d, (x + 30, 721), subtitle, 31, fg)
    for x in (655, 1265):
        arrow(d, x, 674)
    # A travelling pulse makes the direct texture path tangible.
    d.line((110, 851, 1810, 851), fill=LINE, width=4)
    for k in range(7):
        x = 110 + ((time * .27 + k / 7) % 1) * 1700
        d.ellipse((x - 7, 844, x + 7, 858), fill=YELLOW)
    footer(d, ['GPU textures flow straight into AMD’s encoder.', 'WGC capture → Radeon compute → native encoding → Moonlight'])


def comparison(d, time, fresh=False):
    label(d, 'MEASURED ON RADEON · CONTROLLED GPU LOAD')
    text(d, (76, 257), 'More fresh frames. More game.' if fresh else 'Less delay. More in the moment.',
         95, bold=True)
    text(d, (84, 404), 'Fresh pictures per second' if fresh else 'Render → decoded picture · average milliseconds', 37, MUTED)
    p = ease(time / 1.35)
    vals = (23.9, 51.4) if fresh else (96.4, 42.4)
    max_value = 55 if fresh else 100
    for i, (name, value) in enumerate(zip(('Vibepollo 2.0', 'Butterpollo rc.2'), vals)):
        y = 510 + i * 177
        text(d, (84, y), name, 34, WHITE, i == 1)
        d.rounded_rectangle((84, y + 60, 1075, y + 124), radius=10, fill=PANEL)
        length = max(16, 990 * value / max_value * p)
        d.rounded_rectangle((84, y + 60, 84 + length, y + 124), radius=10,
                            fill=YELLOW if i else (102, 115, 140))
        text(d, (1100, y + 70), f'{value:.1f}', 38, YELLOW if i else WHITE, True)
    metric = '2.15×' if fresh else '56%'
    text(d, (1540, 599), metric, 163 if fresh else 200, YELLOW, True, anchor='mm')
    text(d, (1540, 740), 'AS MANY FRESH PICTURES' if fresh else 'LOWER PICTURE DELAY',
         28, WHITE, True, anchor='mm')
    footer(d, ['RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps · three runs per host',
               'Butterpollo rc.2 vs Vibepollo 2.0 · October 4, 2026 · Full results: rust/PERFORMANCE.md'])


def scene_hdr(d, time):
    label(d, 'LATEST rc.10 · NATIVE HDR VALIDATION')
    text(d, (76, 255), 'Native HDR. Verified colour.', 114, bold=True)
    # A spectrum is a design element; the encoded demo is SDR.
    stops = [(255, 94, 91), (255, 210, 66), (99, 212, 143), (81, 177, 255), (156, 114, 247)]
    for x in range(80, 1840):
        u = (x - 80) / 1760 * 4
        a = min(3, int(u))
        f = u - a
        color = tuple(int(stops[a][j] * (1 - f) + stops[a + 1][j] * f) for j in range(3))
        d.line((x, 460, x, 472), fill=color)
    items = [(80, '~60', 'DISTINCT PICTURES / SEC', 'Fresh images at the target rate'),
             (680, '5,173', 'HDR FRAMES DECODED', 'HEVC + AV1 · four complete runs'),
             (1280, '<0.51', 'MEAN ABS. COLOUR ERROR', 'Of one 10-bit code value')]
    for x, metric, caption, detail in items:
        d.rounded_rectangle((x, 524, x + 560, 804), radius=25, fill=PANEL, outline=LINE, width=2)
        text(d, (x + 32, 550), metric, 112, YELLOW, True)
        text(d, (x + 34, 697), caption, 27, WHITE, True)
        text(d, (x + 34, 746), detail, 25, MUTED)
    text(d, (83, 861), 'Native capture. Independent decoding. Verified pixels.', 38)
    footer(d, ['rc.10 · RX 7900 XT · native virtual HDR · 1280×720/60 · WGC + AMD AMF',
               'HEVC and AV1 · FP16 capture · October 6, 2026 · Full results: rust/PERFORMANCE.md'])


def scene_features(d, time):
    label(d, 'BUILT FOR YOUR WHOLE SETUP')
    text(d, (76, 254), 'Your games. Your screens.', 120, bold=True)
    features = [('AV1 + HEVC HDR', 'Sharp, native AMD encoding'),
                ('Virtual displays', 'A screen for every device'),
                ('RTSS frame control', 'Your frame limit, integrated'),
                ('Steam + Playnite', 'Your library, ready to launch'),
                ('Live stream stats', 'See your stream in real time'),
                ('Easy upgrades', 'Keep settings, pairings + apps')]
    for i, (name, detail) in enumerate(features):
        x = 80 + (i % 3) * 600
        y = 464 + (i // 3) * 185
        d.rounded_rectangle((x, y, x + 560, y + 156), radius=23, fill=PANEL, outline=LINE, width=2)
        d.rounded_rectangle((x + 25, y + 28, x + 32, y + 68), radius=3, fill=YELLOW)
        text(d, (x + 49, y + 24), name, 39, WHITE, True)
        text(d, (x + 49, y + 89), detail, 28, MUTED)
    text(d, (82, 866), '263 automated tests passed', 40, YELLOW, True)
    text(d, (1095, 872), 'Moonlight PC 6.2.0 tested', 35)
    footer(d, ['6,342 frames decoded cleanly across five latest rc.10 SDR + HDR validation runs.',
               'Updates notify you first. Automatic installation is opt-in.'])


def scene_pyrowave(d, time):
    label(d, 'PYROWAVE · FULL HDR 4:4:4 SUPPORT')
    text(d, (76, 251), 'Full colour. Every pixel.', 128, bold=True)
    text(d, (82, 435), '10-bit HDR + full-resolution chroma.', 48)
    # A colour-sampling diagram, not an encoded image-quality comparison.
    colors = [(255, 99, 109), (255, 185, 90), (96, 214, 158), (94, 163, 247)]
    for row in range(4):
        for col in range(4):
            x, y = 86 + col * 73, 555 + row * 73
            color = colors[(col + row) % 4]
            d.rounded_rectangle((x, y, x + 62, y + 62), radius=10, fill=color)
            # Every position carries its own chroma sample.
            d.ellipse((x + 26, y + 26, x + 36, y + 36), fill=BG)
    text(d, (428, 580), 'Colour data at every pixel.', 47, bold=True)
    text(d, (430, 659), 'Crisp coloured text.', 39, MUTED)
    text(d, (430, 716), 'Fine edges. Rich HDR highlights.', 39, MUTED)
    text(d, (1525, 641), '4:4:4', 183, YELLOW, True, anchor='mm')
    text(d, (1525, 775), 'FULL-RESOLUTION COLOUR', 27, WHITE, True, anchor='mm')
    footer(d, ['PyroWave + Nonary’s Moonlight client · fast wired LAN',
               'HDR 4:4:4 encoding + encrypted transport validated · Setup: rust/README.md'])


def scene_cta(d, time):
    label(d, 'YOUR NEXT STREAM STARTS HERE')
    text(d, (76, 256), 'You’ve got Radeon.', 126, bold=True)
    text(d, (76, 412), 'Now get Butterpollo.', 126, YELLOW, True)
    text(d, (83, 607), 'Install. Pair Moonlight. Play.', 58)
    pill(d, 82, 728, 'DOWNLOAD rc.10', YELLOW, BG, 38)
    text(d, (83, 836), 'github.com/RamazanKara/Butterpollo', 42, WHITE, bold=True)
    footer(d, ['Open source. Built in Rust. Made for AMD.', '“Smooth as butter.” — early tester'])


SCENE_DRAWERS = [scene_hero, scene_compute, comparison,
                 lambda d, t: comparison(d, t, True), scene_hdr, scene_pyrowave,
                 scene_features, scene_cta]


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
    args = parser.parse_args()
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
