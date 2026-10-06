#!/usr/bin/env python3
"""A 60-second Butterpollo launch film, rendered deterministically with Pillow.

No service, game, display or GPU access. Requires Pillow and FFmpeg.
The continuous gold frame connects explanatory motion and measured evidence.
Timing in the queue diagram is schematic; all numerical claims are fixed.

python3 docs/media/demo_audio.py --output /tmp/score.wav --duration 60
python3 docs/media/render_launch_film.py --storyboard /tmp/film.jpg
python3 docs/media/render_launch_film.py --output /tmp/film.mp4 --audio /tmp/score.wav
python3 docs/media/render_launch_film.py --frame 21 --output /tmp/frame.png

The optional original score generator requires NumPy. The film itself requires
Pillow and FFmpeg only. All visuals are explanatory motion graphics; no rendered
sequence is presented as recorded gameplay or a real-time benchmark.
"""
import argparse
from functools import lru_cache
import math
from pathlib import Path
import shutil
import subprocess

from PIL import Image, ImageDraw, ImageFont

W, H, DURATION = 1920, 1080, 60
BG = (10, 13, 19)
INK = (246, 246, 242)
GOLD = (255, 209, 72)
MUTED = (163, 174, 190)
DIM = (84, 97, 114)
RULE = (42, 52, 66)
PANEL = (21, 28, 39)
TEAL = (118, 208, 203)
STARTS = (0, 5, 16, 26, 38, 46, 54)
ENDS = STARTS[1:] + (60,)
CHAPTERS = ('WRITTEN IN RUST', 'THE COMPUTE PATH', 'COMPUTE OFF / ON',
            'WHOLE-HOST COMPARISON', 'NATIVE HDR VALIDATION', 'PYROWAVE', 'TRY BUTTERPOLLO')


def clamp(x):
    return min(1., max(0., x))


def smooth(x):
    x = clamp(x)
    return x*x*(3-2*x)


def ease(x):
    x = clamp(x)
    return 1-(1-x)**3


def mix(a, b, p):
    return tuple(round(x+(y-x)*clamp(p)) for x, y in zip(a, b))


@lru_cache(maxsize=100)
def font(size, weight='regular'):
    names = {'regular': 'segoeui.ttf', 'bold': 'segoeuib.ttf', 'light': 'segoeuil.ttf', 'mono': 'consola.ttf'}
    for root in (Path('/mnt/c/Windows/Fonts'), Path('C:/Windows/Fonts')):
        p = root/names[weight]
        if p.exists():
            return ImageFont.truetype(str(p), size)
    name = {'regular': 'DejaVuSans.ttf', 'bold': 'DejaVuSans-Bold.ttf',
            'light': 'DejaVuSans.ttf', 'mono': 'DejaVuSansMono.ttf'}[weight]
    return ImageFont.truetype('/usr/share/fonts/truetype/dejavu/'+name, size)


@lru_cache(maxsize=500)
def lettering(value, size, color, weight):
    f = font(size, weight)
    box = f.getbbox(value)
    im = Image.new('RGBA', (max(1, box[2]-box[0]+4), max(1, box[3]-box[1]+4)))
    ImageDraw.Draw(im).text((2-box[0], 2-box[1]), value, font=f, fill=color)
    return im


def txt(im, x, y, value, size=32, color=INK, weight='regular', align='left', opacity=1):
    if opacity <= 0:
        return
    stamp = lettering(value, size, color, weight)
    if align == 'center':
        x -= stamp.width/2
    elif align == 'right':
        x -= stamp.width
    if opacity < 1:
        stamp = stamp.copy()
        stamp.putalpha(stamp.getchannel('A').point(lambda v: round(v*opacity)))
    im.paste(stamp, (round(x), round(y)), stamp)


def reveal(im, x, y, value, size, local, at=0, color=INK, weight='bold', align='left'):
    p = ease((local-at)/.65)
    txt(im, x, y+24*(1-p), value, size, color, weight, align, p)


def line(im, xy, fill=RULE, width=2):
    ImageDraw.Draw(im).line(xy, fill=fill, width=width)


def rect(im, box, fill=PANEL, outline=None, width=1, radius=8):
    ImageDraw.Draw(im).rounded_rectangle(tuple(round(v) for v in box), radius=radius,
                                        fill=fill, outline=outline, width=width)


def arrow(im, x0, y0, x1, y1, color=GOLD, width=3):
    line(im, (x0, y0, x1, y1), color, width)
    angle = math.atan2(y1-y0, x1-x0)
    points = [(x1, y1)] + [(x1-13*math.cos(angle+a), y1-13*math.sin(angle+a)) for a in (-.45, .45)]
    ImageDraw.Draw(im).polygon(points, fill=color)


def foot(im, *lines):
    for i, value in enumerate(lines):
        txt(im, 104, 951+i*37, value, 28 if i == 0 else 26, MUTED)


@lru_cache(maxsize=1)
def background():
    im = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(im)
    # Very low-contrast vertical depth. No synthetic footage or decoration.
    for y in range(H):
        d.line((0, y, W, y), fill=mix(BG, (16, 22, 31), y/H*.65))
    return im


def title(im, value, local, subtitle=None):
    reveal(im, 100, 175, value, 76, local, at=.15)
    if subtitle:
        reveal(im, 104, 279, subtitle, 31, local, at=.3, color=MUTED, weight='regular')


def heading(im, index, t):
    alpha = 1-smooth((t-53.7)/.6)
    if alpha <= 0:
        return
    rect(im, (104, 61, 143, 100), mix(BG, GOLD, alpha), radius=10)
    txt(im, 123, 64, 'B', 28, BG, 'bold', 'center', alpha)
    txt(im, 158, 63, 'Butterpollo', 31, INK, 'bold', opacity=alpha)
    txt(im, 1816, 69, CHAPTERS[index], 22, MUTED, 'mono', 'right', alpha)


def intro(t):
    t += .85  # The first frame already carries the audience hook.
    im = background().copy()
    reveal(im, 102, 205, 'Your Radeon.', 108, t, at=0)
    reveal(im, 102, 334, 'A more responsive stream.', 94, t, at=.25)
    reveal(im, 108, 477, 'Written in Rust.', 39, t, at=.6, color=GOLD)
    reveal(im, 433, 481, 'Windows game streaming for Moonlight.', 35, t, at=.6,
           color=MUTED, weight='regular')
    txt(im, 108, 597, 'AVERAGE RENDER → DECODE', 25, MUTED, 'mono', opacity=ease((t-.7)/.5))
    reveal(im, 100, 661, '41.0', 144, t, at=.75, color=(144, 155, 172), weight='light')
    p = ease((t-1)/.6)
    if p:
        arrow(im, 491, 742, 491+145*p, 742, mix(BG, GOLD, p), 4)
    reveal(im, 685, 661, '33.5', 144, t, at=.85, color=GOLD)
    reveal(im, 1006, 752, 'ms', 48, t, at=.85, color=GOLD, weight='regular')
    # A labelled desktop frame is the same object followed throughout the film.
    txt(im, 1575, 850, 'FOLLOW THE FRAME', 24, MUTED, 'mono', 'center', ease((t-1.2)/.5))
    foot(im, 'Same-build compute off / on · RX 7900 XT · controlled GPU load',
         '1080p60 HEVC HDR · render-to-decode measurement · full comparison follows')
    return im


def job(im, x0, x1, y, name, color, cursor, small=False):
    h = 54
    rect(im, (x0, y, x1, y+h), PANEL, RULE, radius=5)
    p = clamp((cursor-x0)/(x1-x0))
    if p:
        rect(im, (x0, y, x0+max(5, (x1-x0)*p), y+h), mix(PANEL, color, .65), radius=5)
    txt(im, (x0+x1)/2, y+13, name, 24 if small else 26,
        INK if color != GOLD else mix(INK, BG, p), 'bold', 'center')


def gate(im, x, y, active):
    color = TEAL if active else DIM
    line(im, (x, y-13, x, y+13), color, 3)
    ImageDraw.Draw(im).polygon([(x, y-5), (x+5, y), (x, y+5), (x-5, y)], fill=color)


def queue_cursor(local):
    return 420+1280*clamp((local-.7)/7.6)


def queue_frame(local):
    cursor = queue_cursor(local)
    return min(cursor, 1190), 849+77*clamp((cursor-810)/50), 40


def schedule(t):
    im = background().copy()
    title(im, 'Prepare the frame alongside the game.', t,
          'GPU copies + colour conversion move to D3D12 compute.')
    cursor = queue_cursor(t)
    txt(im, 104, 356, 'OTHER SUNSHINE HOSTS · D3D11', 27, INK, 'bold')
    txt(im, 1816, 359, 'Reviewed path: Vibepollo 2.0', 26, MUTED, align='right')
    txt(im, 104, 434, 'Graphics', 26, MUTED)
    txt(im, 104, 522, 'Native AMF', 26, MUTED)
    for x0, x1 in ((420, 640), (660, 880)):
        job(im, x0, x1, 424, 'Game draw', DIM, cursor)
    job(im, 902, 1062, 424, 'Copy', GOLD, cursor)
    job(im, 1082, 1292, 424, 'RGB → YUV', GOLD, cursor)
    line(im, (1298, 451, 1320, 451, 1320, 540, 1336, 540), DIM, 2)
    gate(im, 1320, 491, cursor >= 1292)
    job(im, 1340, 1570, 512, 'Encode', (67, 133, 159), cursor)
    arrow(im, 1578, 539, 1637, 539, TEAL if cursor >= 1570 else DIM, 2)
    rect(im, (1647, 512, 1815, 566), PANEL, GOLD if cursor >= 1570 else RULE, radius=5)
    txt(im, 1731, 526, 'Bitstream', 25, GOLD if cursor >= 1570 else MUTED, 'bold', 'center')
    line(im, (104, 605, 1816, 605), RULE, 1)
    txt(im, 104, 630, 'BUTTERPOLLO · D3D12 COMPUTE', 27, GOLD, 'bold')
    txt(im, 104, 705, 'Graphics', 26, MUTED)
    txt(im, 104, 786, 'Compute', 26, GOLD)
    txt(im, 104, 859, 'Native AMF', 26, MUTED)
    for x0, x1 in ((420, 640), (660, 880)):
        job(im, x0, x1, 695, 'Game draw', DIM, cursor)
    job(im, 420, 580, 776, 'Copy', GOLD, cursor)
    job(im, 600, 810, 776, 'RGB → YUV', GOLD, cursor)
    gate(im, 402, 803, t >= .7)
    gate(im, 590, 803, cursor >= 580)
    line(im, (818, 803, 839, 803, 839, 876, 854, 876), DIM, 2)
    gate(im, 839, 845, cursor >= 810)
    job(im, 860, 1090, 849, 'Encode', (67, 133, 159), cursor)
    arrow(im, 1098, 876, 1140, 876, TEAL if cursor >= 1090 else DIM, 2)
    if t > 5:
        txt(im, 1255, 853, 'Ready for the stream.', 31, GOLD, 'bold', opacity=ease((t-5)/.5))
    if t > 8:
        txt(im, 1255, 896, 'AMF releases the surface for reuse.', 23, MUTED, opacity=ease((t-8)/.5))
    txt(im, 1540, 705, 'Output texture pool', 24, MUTED, align='center')
    for i in range(3):
        rect(im, (1476+i*47,750,1509+i*47,777), PANEL,
             TEAL if t >= 8 else RULE, 2, radius=3)
    if t >= 8:
        p=smooth((t-8)/1.2)
        x=1190+(1492-1190)*p
        y=916+(763-916)*p-70*math.sin(p*math.pi)
        rect(im,(x-13,y-10,x+13,y+10),PANEL,TEAL,2,radius=3)
    foot(im, 'Scheduling schematic · equal game and encode work · graphics and compute share GPU resources',
         'GPU textures in both paths · readiness fences protect copies and the encoder handoff')
    return im


def chart(im, x, y, width, heading_text, values, t, delay=0):
    txt(im, x, y, heading_text, 35, INK, 'bold')
    for i, (value, caption) in enumerate(zip(values, ('Compute off', 'Compute on'))):
        yy = y+90+i*171
        color = GOLD if i else (138, 153, 174)
        p = ease((t-delay-.4-i*.9)/1.2)
        txt(im, x, yy+13, caption, 29, MUTED)
        if p:
            txt(im, x+width, yy-1, f'{value:.1f} ms', 53, color, 'bold', 'right')
        rect(im, (x, yy+70, x+width, yy+108), PANEL, radius=4)
        if p:
            rect(im, (x, yy+70, x+max(5,width*value/60*p), yy+108), color, radius=4)
    line(im, (x, y+399, x+width, y+399), RULE, 2)
    for v in (0, 20, 40, 60):
        xx = x+width*v/60
        line(im, (xx, y+393, xx, y+405), MUTED, 1)
        txt(im, xx, y+418, str(v), 23, MUTED, 'regular', 'center')


def compute(t):
    im = background().copy()
    title(im, 'The compute change, measured.', t,
          'Same Butterpollo build. Compute off → compute on.')
    chart(im, 104, 392, 725, 'Average picture delay', (41., 33.5), t, .4)
    chart(im, 1091, 392, 725, 'Slower frames · 95th percentile', (54.4, 42.3), t, 2.5)
    txt(im, 104, 871, 'Frame rendered → picture decoded', 29, INK)
    txt(im, 1816, 871, 'Matching 0–60 ms scales', 27, MUTED, align='right')
    foot(im, 'RX 7900 XT · DDX · 1080p60 HEVC HDR · controlled GPU load · two runs per path',
         'October 4, 2026 · encrypted loopback · arithmetic means · methods in rust/PERFORMANCE.md')
    return im


def result_line(im, x, y, title_text, before, after, local, at):
    p = ease((local-at)/.7)
    txt(im, x, y, title_text, 28, MUTED, opacity=p)
    txt(im, x-3, y+47, before, 74, (143, 155, 174), 'light', opacity=p)
    if p:
        arrow(im, x+287, y+95, x+386, y+95, mix(BG, GOLD, p), 3)
    txt(im, x+423, y+47, after, 74, GOLD, 'bold', opacity=p)
    txt(im, x+758, y+85, 'ms', 32, GOLD, opacity=p)


def hosts(t):
    im = background().copy()
    title(im, 'Butterpollo vs other Sunshine hosts.', t,
          'Historical whole-host comparison · 1080p60 HEVC HDR')
    reveal(im, 91, 373, '56%', 205, t, at=.6, color=GOLD)
    reveal(im, 105, 616, 'lower average picture delay', 34, t, at=.9,
           color=INK, weight='regular')
    result_line(im, 966, 396, 'Average · render → decode', '96.4', '42.4', t, .6)
    result_line(im, 966, 600, '95th percentile · render → decode', '137.0', '56.5', t, 2.2)
    line(im, (104, 776, 1816, 776), RULE, 1)
    p = ease((t-5)/.6)
    txt(im, 101, 811, '2.15×', 79, GOLD, 'bold', opacity=p)
    txt(im, 388, 824, 'as many fresh pictures', 36, INK, opacity=p)
    txt(im, 1816, 825, '23.9 → 51.4 FPS', 43, GOLD, 'bold', 'right', p)
    foot(im, 'RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps requested · controlled GPU load',
         'Vibepollo 2.0 → Butterpollo rc.2 · October 4, 2026 · three runs per host · rust/PERFORMANCE.md')
    return im


def hdr(t):
    im = background().copy()
    title(im, 'HDR, checked after decoding.', t,
          'Native capture. Native AMD encoding. Independent pixel checks.')
    stages = [('FP16 scRGB', 'Capture'), ('10-bit BT.2020 / PQ', 'Colour conversion'),
              ('HEVC + AV1', 'Native AMF'), ('Decoded pixels', 'Compare to reference')]
    for i, (head, note) in enumerate(stages):
        x = 104+i*440
        active = t >= .6+i*.85
        line(im, (x, 389, x+350, 389), GOLD if active else RULE, 3)
        txt(im, x, 418, head, 30, INK if active else MUTED, 'bold')
        txt(im, x, 477, note, 27, MUTED)
        if i < 3:
            arrow(im, x+370, 472, x+415, 472, TEAL if t >= 1.2+i*.85 else DIM, 2)
    reveal(im, 98, 644, '5,173 / 5,173', 133, t, at=2.7, color=GOLD)
    reveal(im, 107, 812, 'frames decoded across four native HDR runs', 39, t, at=2.9,
           color=INK, weight='regular')
    foot(im, 'rc.10 · RX 7900 XT · native FP16 virtual HDR capture · HEVC + AV1 · 1280×720/60',
         'Decoded BT.2020 / PQ and reference colours verified · results in rust/PERFORMANCE.md')
    return im


def sample_rgb(y, cb, cr):
    return tuple(round(clamp(c)*255) for c in (y+1.5748*cr,
                 y-.1873*cb-.4681*cr, y+1.8556*cb))


def chroma_block(im, x, y, full, t):
    # The two diagrams have identical luma values. Only chroma sample count changes.
    lumas = (.38, .62, .62, .38)
    # Nonzero mean chroma keeps the 4:2:0 example coloured too. Subsampling
    # merges local colour differences; it does not generally remove colour.
    chromas = ((.10, .21), (-.16, -.13), (.10, -.13), (-.16, .21))
    for i, yy in enumerate(lumas):
        xx, dy = i % 2, i//2
        cb, cr = chromas[i] if full else (-.03, .04)
        p = ease((t-.7-i*.22)/.6)
        color = sample_rgb(yy, cb*p, cr*p)
        rect(im, (x+xx*152, y+dy*152, x+xx*152+144, y+dy*152+144), color, radius=4)
        # Colour sample markers make the sampling count explicit, independent of colour vision.
        if full:
            ImageDraw.Draw(im).ellipse((x+xx*152+65, y+dy*152+65, x+xx*152+79, y+dy*152+79), fill=BG)
    if not full:
        ImageDraw.Draw(im).ellipse((x+137, y+137, x+159, y+159), fill=GOLD, outline=BG, width=3)


def pyrowave(t):
    im = background().copy()
    title(im, 'Colour detail. At every pixel.', t,
          'PyroWave · full 10-bit HDR 4:4:4')
    txt(im, 356, 364, '4:2:0', 47, MUTED, 'bold', 'center')
    txt(im, 1509, 364, '4:4:4', 47, GOLD, 'bold', 'center')
    chroma_block(im, 208, 450, False, t)
    chroma_block(im, 1361, 450, True, t)
    txt(im, 960, 484, 'Same 2×2 pixels.', 37, INK, 'bold', 'center')
    txt(im, 960, 543, 'Same luma detail.', 30, MUTED, 'regular', 'center')
    txt(im, 960, 605, 'Four colour samples.', 37, GOLD, 'bold', 'center')
    arrow(im, 655, 693, 1235, 693, mix(RULE, GOLD, ease((t-1.7)/1.3)), 3)
    txt(im, 356, 776, '1 Cb + 1 Cr', 30, MUTED, 'regular', 'center')
    txt(im, 1509, 776, '4 Cb + 4 Cr', 30, GOLD, 'bold', 'center')
    reveal(im, 960, 859, 'Full-resolution colour for fine text and edges.', 38, t,
           at=2.5, color=INK, weight='regular', align='center')
    foot(im, 'Chroma sampling schematic · PyroWave uses its own shared D3D11 / Vulkan path',
         'Use Nonary’s compatible Moonlight client on a fast wired LAN')
    return im


def ending(t):
    im = background().copy()
    reveal(im, 409, 236, 'Butterpollo', 148, t, at=.1)
    reveal(im, 414, 425, 'Written in Rust. Built for Radeon. Made for Moonlight.', 39, t, at=.4,
           color=MUTED, weight='regular')
    for i, (x, value) in enumerate(((104, 'Install.'), (686, 'Pair Moonlight.'), (1505, 'Play.'))):
        reveal(im, x, 630, value, 55, t, at=.65+i*.25, color=GOLD)
    reveal(im, 960, 812, 'github.com/RamazanKara/Butterpollo', 55, t, at=1.35,
           color=INK, weight='bold', align='center')
    txt(im, 960, 937, 'WGC + Radeon compute by default · Free and open source', 30,
        MUTED, 'regular', 'center', ease((t-1.8)/.65))
    return im


DRAWERS = (intro, schedule, compute, hosts, hdr, pyrowave, ending)

# A single frame tile moves through the film. It leaves the schematic before
# becoming a measurement marker; it does not imply schematic durations are data.
KEYS = ((0,1575,710,220), (4.15,1575,710,220), (5.8,*queue_frame(.8)),
        (14.7,*queue_frame(9.7)), (16.9,104,742,25), (18.9,508.79,742,25),
        (19.1,508.79,742,25), (19.8,1091,742,25),
        (21.1,1602.13,742,25), (25,1602.13,742,25), (27.1,1557,572,30),
        (30,1557,572,30), (33.5,1567,775,26), (36.5,1768,779,26),
        (38.8,277,554,65), (40,717,554,65), (41.2,1157,554,65),
        (42.7,1597,554,65), (46.8,1597,554,65), (47.7,1509,598,324),
        (53.9,1509,598,324), (55.1,268,333,170),
        (60,268,333,170))


def tracked_frame(im, t):
    left, right = KEYS[0], KEYS[-1]
    for a, b in zip(KEYS, KEYS[1:]):
        if a[0] <= t < b[0]:
            left, right = a, b
            break
    p = smooth((t-left[0])/max(.0001, right[0]-left[0]))
    x, y, size = [a+(b-a)*p for a, b in zip(left[1:], right[1:])]
    if 5.8 <= t <= 14.7:
        x,y,size=queue_frame(t-5)
    elif 16.9 <= t <= 19.1:
        x=104+725*33.5/60*ease((t-16-1.7)/1.2)
        y,size=742,25
    elif 19.8 <= t <= 25:
        x=1091+725*42.3/60*ease((t-16-3.8)/1.2)
        y,size=742,25
    d = ImageDraw.Draw(im)
    box = (x-size/2, y-size/2, x+size/2, y+size/2)
    logo = smooth((t-54.25)/.9)
    macro = 46.8 <= t < 54
    d.rounded_rectangle(box, radius=max(5, round(size*.14)), fill=None if macro else mix(PANEL,GOLD,logo),
                        outline=GOLD, width=max(2,round(size*.02)))
    if logo < 1 and not macro:
        for i, frac in enumerate((.58,.78,.46)):
            yy = y-size*.22+i*size*.22
            line(im, (x-size*.31, yy, x-size*.31+size*frac, yy), mix(PANEL,GOLD,1-logo), max(2,round(size*.045)))
    if logo > 0:
        txt(im, x, y-size*.40, 'B', max(18,round(size*.72)), BG, 'bold', 'center', logo)


def plate(index, local):
    return DRAWERS[index](max(0,local))


def frame(t):
    index = next((i for i, end in enumerate(ENDS) if t < end), len(ENDS)-1)
    local = t-STARTS[index]
    if index and local < .8:
        # Follow the frame across a continuous horizontal canvas. The scene
        # stays fully lit; outgoing and incoming text never double-expose.
        old = plate(index-1, ENDS[index-1]-STARTS[index-1]-.001)
        new = plate(index, local)
        offset=round(W*smooth(local/.8))
        im=background().copy()
        im.paste(old,(-offset,0))
        im.paste(new,(W-offset,0))
    else:
        im = plate(index, local)
    heading(im, index, t)
    tracked_frame(im, t)
    return im


def storyboard(destination):
    times = (.8,3.5,7,10,14,18.5,23,28.5,33,36,40,44,48,52,56,59)
    sheet = Image.new('RGB',(1920,4*588),BG)
    for i,t in enumerate(times):
        tile=frame(t).resize((480,270),Image.Resampling.LANCZOS)
        x,y=i%4*480,i//4*588
        # Each row includes a second, larger-text inspection strip of its scope.
        sheet.paste(tile,(x,y))
        txt(sheet,x+12,y+284,f'{t:04.1f}s',25,MUTED,'mono')
        crop=frame(t).crop((60,925,1860,1040)).resize((480,31),Image.Resampling.LANCZOS)
        sheet.paste(crop,(x,y+327))
    # Compact the empty row spacing while preserving 16 representative frames.
    compact = Image.new('RGB',(1920,4*380),BG)
    for row in range(4):
        compact.paste(sheet.crop((0,row*588,1920,row*588+380)),(0,row*380))
    destination.parent.mkdir(parents=True,exist_ok=True)
    compact.save(destination)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path)
    p.add_argument('--audio',type=Path)
    p.add_argument('--frame',type=float)
    p.add_argument('--storyboard',type=Path)
    p.add_argument('--fps',type=int,default=60)
    p.add_argument('--width',type=int,default=1920)
    args=p.parse_args()
    if args.storyboard:
        storyboard(args.storyboard)
    if args.output and args.frame is not None:
        args.output.parent.mkdir(parents=True,exist_ok=True)
        frame(args.frame).save(args.output)
    elif args.output:
        ffmpeg=shutil.which('ffmpeg')
        if not ffmpeg:
            p.error('FFmpeg is required.')
        args.output.parent.mkdir(parents=True,exist_ok=True)
        vf=f'scale={args.width}:-2:flags=lanczos:out_color_matrix=bt709:out_range=tv'
        cmd=[ffmpeg,'-hide_banner','-loglevel','error','-y','-f','rawvideo','-pixel_format','rgb24',
             '-video_size',f'{W}x{H}','-framerate',str(args.fps),'-i','pipe:0']
        if args.audio:
            cmd+=['-i',str(args.audio),'-map','0:v','-map','1:a','-c:a','aac','-b:a','192k']
        else:
            cmd+=['-an']
        cmd+=['-vf',vf,'-c:v','libx264','-threads','4','-preset','medium','-crf','19',
              '-pix_fmt','yuv420p','-movflags','+faststart','-color_primaries','bt709',
              '-color_trc','bt709','-colorspace','bt709','-color_range','tv',
              '-t',str(DURATION),str(args.output)]
        with subprocess.Popen(cmd,stdin=subprocess.PIPE) as proc:
            try:
                for n in range(DURATION*args.fps):
                    proc.stdin.write(frame(n/args.fps).tobytes())
                    if n%(args.fps*5)==0:
                        print(f'Rendered {n//args.fps}/{DURATION}s',flush=True)
            finally:
                proc.stdin.close()
            if proc.wait():
                raise RuntimeError('FFmpeg encoding failed')
        print(f'Wrote {args.output}',flush=True)
    elif not args.storyboard:
        p.error('Use --output or --storyboard.')


if __name__=='__main__':
    main()
