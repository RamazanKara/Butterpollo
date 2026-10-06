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
DURATION = 57
BG = (13, 16, 22)
WHITE = (248, 249, 252)
YELLOW = (255, 210, 66)
MUTED = (164, 173, 191)
PANEL = (24, 29, 39)
LINE = (48, 56, 70)
SCENES = ((0, 18), (18, 26), (26, 35), (35, 43), (43, 51), (51, 57))


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


def clamp(value):
    return max(0.0, min(1.0, value))


def mix(a, b, amount):
    return tuple(round(x + (y - x) * clamp(amount)) for x, y in zip(a, b))


def stage_job(d, x0, x1, y, title, color, cursor, is_frame=True):
    bottom = y + 58
    d.rounded_rectangle((x0, y, x1, bottom), radius=9, fill=PANEL, outline=mix(LINE, color, .4), width=2)
    fill = clamp((cursor - x0) / (x1 - x0))
    if fill:
        d.rounded_rectangle((x0, y, max(x0 + 10, x0 + (x1 - x0) * fill), bottom), radius=9, fill=mix(PANEL, color, .7))
    active = x0 <= cursor < x1
    if active:
        d.rounded_rectangle((x0, y, x1, bottom), radius=9, outline=WHITE, width=2)
    foreground = BG if cursor >= x1 and color == YELLOW else WHITE
    text(d, ((x0 + x1) / 2, y + 18), title, 26, foreground, True, anchor='mm')
    state = 'F042' if active and is_frame else ('rendering' if active else ('complete' if cursor >= x1 else 'queued'))
    text(d, ((x0 + x1) / 2, y + 44), state, 20, foreground if active or cursor >= x1 else MUTED, anchor='mm')


def gate(d, x, y, ready):
    color = (125, 217, 189) if ready else (97, 110, 128)
    d.line((x, y - 21, x, y + 21), fill=color, width=3)
    d.polygon([(x, y - 6), (x + 6, y), (x, y + 6), (x - 6, y)], fill=color)


def frame_output(d, x, y, ready):
    color = YELLOW if ready else LINE
    d.rounded_rectangle((x, y, x + 145, y + 58), radius=9, fill=PANEL, outline=color, width=2)
    text(d, (x + 72, y + 19), 'F042', 28, color, True, anchor='mm')
    text(d, (x + 72, y + 45), 'bitstream' if ready else 'output', 19, color, anchor='mm')


def scene_schedule(d, time):
    time = 6.7 if time is None else time
    producer_ready = time >= 2.3
    cursor = 405 + clamp((time - 3) / 9) * (1660 - 405)
    label(d, 'FOLLOW FRAME 042 / TWO ALTERNATIVE PATHS')
    text(d, (76, 238), 'Move frame preparation onto compute.', 77, bold=True)
    subtitle = ('AMF releases each output texture before it returns to the pool.' if time >= 14
                else 'The game work and hardware-encode duration are identical in this schematic.')
    text(d, (82, 345), subtitle, 34, YELLOW if time >= 14 else MUTED)
    game = ((430, 690), (710, 985))
    stages = [dict(top=406, title='REVIEWED D3D11 PATH / VIBEPOLLO 2.0',
                   graphics=473, prep=473, encode=567, copy=(1015,1195), convert=(1220,1440), enc=(1470,1640), output=1665, release=14),
              dict(top=676, title='BUTTERPOLLO / D3D12 COMPUTE',
                   graphics=738, prep=806, encode=874, copy=(430,610), convert=(635,855), enc=(885,1055), output=1080, release=8.7)]
    for i, p in enumerate(stages):
        title_color = WHITE if i == 0 else YELLOW
        text(d, (82, p['top']), p['title'], 29, title_color, True)
        if not producer_ready:
            status = 'F042: waiting for capture'
        elif cursor < p['copy'][0]:
            status = 'F042: waiting for graphics' if i == 0 else 'F042: ready for compute'
        elif cursor < p['copy'][1]:
            status = 'F042: copying'
        elif cursor < p['convert'][0]:
            status = 'F042: copy-complete fence'
        elif cursor < p['convert'][1]:
            status = 'F042: converting'
        elif cursor < p['enc'][0]:
            status = 'F042: texture ready'
        elif cursor < p['enc'][1]:
            status = 'F042: encoding'
        else:
            status = 'F042: encoded / S3 held' if time < p['release'] else 'F042: encoded / S3 released'
        text(d, (1838, p['top'] + 16), status, 27, YELLOW if i else WHITE, mono=True, anchor='rm')
        text(d, (84, p['graphics'] + 29), 'Graphics work', 27, MUTED, anchor='lm')
        for x0,x1 in game:
            stage_job(d, x0, x1, p['graphics'], 'Game draw', (87,102,124), cursor, False)
        if i:
            text(d, (84, p['prep'] + 29), 'Compute queues', 27, YELLOW, anchor='lm')
        text(d, (84, p['encode'] + 29), 'AMF / hardware', 27, (152,206,219), anchor='lm')
        stage_job(d, *p['copy'], p['prep'], 'GPU copies', YELLOW, cursor)
        stage_job(d, *p['convert'], p['prep'], 'RGB → YUV', YELLOW, cursor)
        stage_job(d, *p['enc'], p['encode'], 'Encode', (71,147,174), cursor)
        gate(d, 405, p['prep'] + 29, producer_ready)
        gate(d, p['copy'][1] + 12, p['prep'] + 29, cursor >= p['copy'][1])
        mid = p['convert'][1] + 15
        d.line((mid, p['prep'] + 29, mid, p['encode'] + 29, p['enc'][0] - 7, p['encode'] + 29), fill=LINE, width=2)
        gate(d, mid, p['prep'] + 29, cursor >= p['convert'][1])
        path_arrow(d, (p['enc'][1] + 4, p['encode'] + 29, p['output'] - 6, p['encode'] + 29), (152,206,219), 2)
        frame_output(d, p['output'], p['encode'], cursor >= p['enc'][1])
        # Output arrival and texture ownership release are distinct events.
        locked = cursor >= p['convert'][0] and time < p['release']
        ownership = ('S3: owned until AMF releases it' if locked else
                     ('AMF released S3 → reusable' if time >= p['release'] else 'Output surface S3: available'))
        ownership_color = (125, 217, 189) if time >= p['release'] else MUTED
        if i:
            text(d, (1838, p['encode'] + 29), ownership, 25, ownership_color, anchor='rm')
        else:
            text(d, (430, p['encode'] + 29), ownership, 25, ownership_color, anchor='lm')
    footer(d, ['Scheduling schematic · illustrative durations · both paths share the same GPU resources',
               'Capture producer ready → copy complete → per-texture ready → AMF releases surface ownership'])


def axis(d, x, y, width=765):
    d.line((x, y, x + width, y), fill=LINE, width=2)
    for value in (0,20,40,60):
        xx = x + width * value / 60
        d.line((xx, y-4, xx, y+4), fill=MUTED, width=1)
        text(d, (xx, y+22), str(value), 24, MUTED, anchor='mm')


def pair_chart(d, x, y, title, values, time):
    text(d, (x, y), title, 36, WHITE, True)
    for i,(value,caption) in enumerate(zip(values,('Compute off / D3D11','Compute on / D3D12'))):
        yy = y + 80 + i * 145
        text(d, (x, yy), caption, 28, MUTED)
        start = .25 + i * 1.1
        p = ease((time-start)/.8)
        d.rounded_rectangle((x,yy+61,x+765,yy+95), radius=5, fill=PANEL)
        if p:
            text(d, (x+765, yy), f'{value:.1f} ms', 40, YELLOW if i else WHITE, True, anchor='rt')
            d.rounded_rectangle((x,yy+61,x+max(10,765*value/60*p),yy+95), radius=5, fill=YELLOW if i else (125,137,157))
    axis(d,x,y+366)


def scene_compute_results(d, time):
    label(d, 'MEASUREMENT 01 / ISOLATE THE COMPUTE CHANGE')
    text(d, (76, 242), 'Same build. Only compute changes.', 81, bold=True)
    text(d, (82, 354), 'Picture delay: moving test-pattern render → independent decoder', 36, MUTED)
    pair_chart(d,84,442,'Average delay',(41.0,33.5),time)
    pair_chart(d,1050,442,'Slower frames / 95th percentile',(54.4,42.3),time-2.6)
    d.line((970,445,970,825),fill=LINE,width=2)
    text(d,(84,882),'Both charts use the same 0–60 ms scale.',31,MUTED)
    footer(d,['RX 7900 XT · DDX · 1080p60 HEVC HDR · controlled GPU load · mean of two runs per path',
              'October 4, 2026 · encrypted loopback stream · compute off / on · rust/PERFORMANCE.md'])


def scene_host_results(d, time):
    label(d, 'MEASUREMENT 02 / SEPARATE WHOLE-HOST COMPARISON')
    text(d,(76,242),'Vibepollo 2.0 → Butterpollo rc.2',83,bold=True)
    text(d,(82,354),'Same Radeon, matched AMF settings, three alternating runs per host.',35,MUTED)
    text(d,(1120,435),'Vibepollo 2.0',34,WHITE,True,anchor='mm')
    text(d,(1580,435),'Butterpollo rc.2',34,YELLOW,True,anchor='mm')
    d.line((80,474,1840,474),fill=LINE,width=2)
    rows=[('Render → decode / average','96.4 ms','42.4 ms'),
          ('Render → decode / 95th percentile','137.0 ms','56.5 ms'),
          ('Fresh pictures per second','23.9 FPS','51.4 FPS')]
    for i,(title,old,new) in enumerate(rows):
        y=535+i*125
        amount=clamp((time-(.35+i*1.35))/.35)
        if amount:
            text(d,(83,y),title,34,mix(BG,WHITE,amount),anchor='lm')
            text(d,(1120,y),old,55,mix(BG,(171,181,197),amount),True,anchor='mm')
            text(d,(1580,y),new,55,mix(BG,YELLOW,amount),True,anchor='mm')
            if i<2:
                d.line((80,y+63,1840,y+63),fill=mix(BG,LINE,amount),width=1)
    if time>=4.8:
        text(d,(83,871),'56% lower average picture delay',36,YELLOW,True)
        text(d,(1120,871),'2.15× as many fresh pictures',36,YELLOW,True)
    footer(d,['RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps requested · controlled GPU load',
              'Historical comparison · October 4, 2026 · mean of three runs per host · rust/PERFORMANCE.md'])


def scene_hdr(d,time):
    label(d,'CURRENT rc.10 / NATIVE HEVC + AV1 HDR')
    text(d,(76,242),'Check the pixels after decoding.',84,bold=True)
    text(d,(83,357),'Native capture → colour conversion → hardware encode → independent decode',33,MUTED)
    stages=[('FP16 scRGB','HDR capture'),('10-bit PQ','BT.2020 colour'),('HEVC / AV1','Native AMD AMF'),('Decoded pixels','Compare to reference')]
    for i,(title,note) in enumerate(stages):
        x=80+i*465
        active=time>=i*.65
        d.rounded_rectangle((x,431,x+365,572),radius=15,fill=PANEL,outline=YELLOW if active else LINE,width=2)
        text(d,(x+24,449),title,34,WHITE if active else MUTED,True)
        text(d,(x+24,507),note,27,MUTED)
        if i<3:
            path_arrow(d,(x+382,502,x+444,502),YELLOW if time>=(i+1)*.65 else LINE)
    if time>=2.5:
        text(d,(83,620),'AV1 decoded reference frame',36,WHITE,True)
        text(d,(85,676),'10-bit PQ luma / Y′',26,MUTED)
        text(d,(638,676),'Expected',26,MUTED,anchor='mt')
        text(d,(926,676),'Decoded',26,MUTED,anchor='mt')
        d.line((80,719,1044,719),fill=LINE,width=2)
        for row,(name,expected,decoded) in enumerate([('Black','64.00','64.00'),('100-nit white','509.08','509.00')]):
            y=766+row*93
            text(d,(84,y),name,31,WHITE,anchor='lm')
            text(d,(638,y),expected,43,WHITE,True,anchor='mm')
            text(d,(926,y),decoded,43,YELLOW,True,anchor='mm')
        d.line((1120,620,1120,890),fill=LINE,width=2)
    if time>=3.5:
        text(d,(1210,632),'Four HEVC / AV1 HDR runs',30,WHITE)
        text(d,(1206,708),'5,173 / 5,173',61,YELLOW,True)
        text(d,(1210,796),'frames decoded',34,WHITE)
        text(d,(1210,854),'Reference colour error <0.51¹',29,MUTED)
    footer(d,['rc.10 · RX 7900 XT · native virtual HDR · FP16 capture · 1280×720/60 · four HEVC/AV1 runs',
              '¹ Mean absolute error in 10-bit code values, four reference frames · rust/PERFORMANCE.md'])


def sampling_grid(d,x,y,full,plane,visible=True):
    text(d,(x+71,y-35),plane,31,WHITE,True,anchor='mm')
    if not visible:
        return
    base=(198,204,214) if plane=='Y' else ((66,175,210) if plane=='Cb' else (202,108,166))
    if full or plane=='Y':
        for row in range(2):
            for col in range(2):
                delta=(row*2+col)*10
                color=tuple(max(0,c-delta) for c in base)
                xx,yy=x+col*75,y+row*75
                d.rounded_rectangle((xx,yy,xx+66,yy+66),radius=8,fill=color)
                d.ellipse((xx+29,yy+29,xx+37,yy+37),fill=BG)
    else:
        d.rounded_rectangle((x,y,x+142,y+142),radius=8,fill=base)
        d.ellipse((x+67,y+67,x+75,y+75),fill=BG)


def scene_pyrowave(d,time):
    label(d,'PYROWAVE / FULL 10-BIT HDR 4:4:4')
    text(d,(76,242),'Keep the colour detail at every pixel.',78,bold=True)
    text(d,(83,353),'Chroma sampling in the same 2×2-pixel block',36,MUTED)
    for i,(mode,detail) in enumerate([('4:2:0','4 Y + 1 Cb + 1 Cr'),('4:4:4','4 Y + 4 Cb + 4 Cr')]):
        x=80+i*940
        d.rounded_rectangle((x,426,x+820,795),radius=17,fill=PANEL,outline=LINE,width=2)
        text(d,(x+31,447),mode,49,YELLOW if i else WHITE,True)
        text(d,(x+785,473),'Full chroma' if i else 'Subsampled chroma',28,MUTED,anchor='rm')
        for j,plane in enumerate(('Y','Cb','Cr')):
            sampling_grid(d,x+111+j*225,575,bool(i),plane, time>=j*.8)
        if time>=1.6:
            text(d,(x+410,757),detail,30,WHITE,anchor='mm')
    if time>=2.2:
        text(d,(83,842),'Full-resolution chroma preserves fine coloured text and edges.',37,YELLOW,True)
    footer(d,['PyroWave: 10-bit planar YUV via shared D3D11 / Vulkan textures · a separate codec path',
              'Use Nonary’s Moonlight client on a fast wired LAN · sampling illustration above'])


def scene_cta(d,time):
    label(d,'BUTTERPOLLO rc.10 / WINDOWS + MOONLIGHT')
    text(d,(76,246),'Prepare the frame alongside the game.',77,bold=True)
    text(d,(83,409),'WGC capture → D3D12 compute → native AMF',45,YELLOW,True)
    text(d,(86,478),'GPU texture handoffs with explicit readiness and ownership fences.',33,MUTED)
    text(d,(83,598),'HEVC + AV1 HDR  /  PyroWave HDR 4:4:4',44,WHITE,True)
    text(d,(86,670),'HEVC and AV1 HDR verified through independent decoded-pixel checks.',32,MUTED)
    text(d,(83,831),'github.com/RamazanKara/Butterpollo',49,YELLOW,True)
    footer(d,['Install → Pair Moonlight → Play',
              'Source, comparison methods and codec validation are linked in the README.'])


SCENE_DRAWERS=[scene_schedule,scene_compute_results,scene_host_results,scene_hdr,scene_pyrowave,scene_cta]

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
    # Clean scene cuts keep the technical labels and numbers readable. Motion
    # within each scene explains the mechanism or reveals the measured values.
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
        sheet = Image.new('RGB', (960 * 2, 540 * ((len(SCENES) + 1) // 2)), BG)
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
