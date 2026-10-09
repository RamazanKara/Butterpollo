#!/usr/bin/env python3
"""Rubylight's 60-second launch film, rendered deterministically with Pillow.

No service, game, display or GPU access. Requires Pillow and FFmpeg.
Diagrams are schematic; measured results name their fixture in the footer.
docs/performance.md and rust/PERFORMANCE.md retain the source runs.

python docs/media/demo_audio.py --output score.wav --duration 60
python docs/media/render_launch_film.py --storyboard film.jpg
python docs/media/render_launch_film.py --output film.mp4 --audio score.wav
python docs/media/render_launch_film.py --frame 21 --output frame.png
"""
import argparse
from functools import lru_cache
import math
from pathlib import Path
import shutil
import subprocess

from PIL import Image, ImageChops, ImageDraw, ImageFont

W, H, DURATION = 1920, 1080, 60
BG = (9, 10, 14)
INK = (244, 243, 238)
RUBY = (255, 64, 108)
RUBY_DEEP = (196, 18, 66)
ROSE = (255, 138, 168)
MINT = (98, 226, 184)
MUTED = (150, 157, 172)
DIM = (68, 74, 90)
RULE = (36, 40, 52)
CARD = (19, 22, 30)
STARTS = (0, 7, 17, 27, 36, 45, 53)
ENDS = STARTS[1:] + (60,)
CHAPTERS = ('MEET RUBYLIGHT', 'THE COMPUTE PATH', 'BESIDE A GAME', 'RADEON COMPUTE',
            'PYROWAVE', 'GET STARTED', 'RUBYLIGHT')
WIPE = .7

# rust/PERFORMANCE_WORK.md, "October 8 real-client picture age over Wi-Fi":
# rc.24, 1968x2184 AV1 HDR 120 fps, render to received on the laptop.
WIFI = {'mean': 13.5, 'p95': 14.3, 'fresh': 120}
# docs/performance.md:57-62; host counter and game rate in
# rust/PERFORMANCE.md:205-221. The host counter is not app-Present latency.
COMPUTE = {'mean': (41.0, 33.5), 'p95': (54.4, 42.3), 'host': (16.2, 11.2), 'game_fps': 174}
# rust/PERFORMANCE_WORK.md, "rc.24 against Vibepollo 2.0 on the same GPU":
# (Vibepollo 2.0, rc.24) beside the 60 fps-capped game, 1968x2184 HDR 120 fps,
# rounded half up to one decimal as on the site.
HOSTS = {'hevc_mean': (27.0, 19.6), 'hevc_p95': (37.6, 21.6),
         'av1_mean': (24.5, 17.2), 'av1_p95': (36.8, 19.0), 'host': (7.4, 3.1)}
# Repeat-frame capacity, not stream latency: rust/PERFORMANCE.md:2138-2152.
PYROWAVE = (.54, .57)

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


FONT_ROOTS = (Path('C:/Windows/Fonts'), Path('/mnt/c/Windows/Fonts'))


@lru_cache(maxsize=None)
def font(size, weight='regular'):
    if weight == 'mono':
        for root in FONT_ROOTS:
            if (root/'consola.ttf').exists():
                return ImageFont.truetype(str(root/'consola.ttf'), size)
        return ImageFont.truetype('/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf', size)
    for root in FONT_ROOTS:
        if (root/'SegUIVar.ttf').exists():
            f = ImageFont.truetype(str(root/'SegUIVar.ttf'), size)
            f.set_variation_by_name({'light': 'Light Display', 'regular': 'Regular Display',
                                     'semibold': 'Semibold Display', 'bold': 'Bold Display'}[weight])
            return f
    name = 'DejaVuSans-Bold.ttf' if weight in ('bold', 'semibold') else 'DejaVuSans.ttf'
    return ImageFont.truetype('/usr/share/fonts/truetype/dejavu/'+name, size)


@lru_cache(maxsize=2000)
def lettering(value, size, color, weight):
    f = font(size, weight)
    box = f.getbbox(value)
    im = Image.new('RGBA', (max(1, box[2]-box[0]+4), max(1, box[3]-box[1]+4)))
    ImageDraw.Draw(im).text((2-box[0], 2-box[1]), value, font=f, fill=color)
    return im


def width_of(value, size, weight='regular'):
    return lettering(value, size, INK, weight).width


def txt(im, x, y, value, size=32, color=INK, weight='regular', align='left', opacity=1.):
    if opacity <= 0 or not value:
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


def wrap(value, size, width, weight='regular'):
    lines, current = [], ''
    for word in value.split():
        trial = (current+' '+word).strip()
        if current and width_of(trial, size, weight) > width:
            lines.append(current)
            current = word
        else:
            current = trial
    return lines+[current] if current else lines


def reveal(im, x, y, value, size, local, at=0., color=INK, weight='bold', align='left', rise=28):
    p = ease((local-at)/.6)
    txt(im, x, y+rise*(1-p), value, size, color, weight, align, p)


def draw(im):
    return ImageDraw.Draw(im)


def rect(im, box, fill=CARD, outline=None, width=1, radius=14):
    draw(im).rounded_rectangle(tuple(round(v) for v in box), radius=radius, fill=fill,
                               outline=outline, width=width)


def line(im, xy, fill=RULE, width=2):
    draw(im).line(tuple(round(v) for v in xy), fill=fill, width=width)


def arrow(im, x0, y0, x1, y1, color=RUBY, width=4):
    line(im, (x0, y0, x1, y1), color, width)
    angle = math.atan2(y1-y0, x1-x0)
    head = [(x1+4*math.cos(angle), y1+4*math.sin(angle))]
    head += [(x1-16*math.cos(angle+a), y1-16*math.sin(angle+a)) for a in (-.5, .5)]
    draw(im).polygon(head, fill=color)


@lru_cache(maxsize=1)
def backdrop():
    im = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(im)
    for y in range(H):
        d.line((0, y, W, y), fill=mix(BG, (14, 16, 23), y/H))
    # A quiet dot grid: a measuring surface, not decoration that moves.
    for y in range(36, H, 48):
        for x in range(36, W, 48):
            d.point((x, y), fill=(29, 32, 42))
    return im


@lru_cache(maxsize=1)
def glow_sprite():
    size = 120
    im = Image.new('RGB', (size, size))
    px = im.load()
    for y in range(size):
        for x in range(size):
            d = math.hypot(x-size/2+.5, y-size/2+.5)/(size/2)
            a = clamp(1-d)**2.4*.30
            c = mix(RUBY_DEEP, RUBY, clamp(1.15-d))
            px[x, y] = tuple(round(v*a) for v in c)
    return im.resize((1500, 1500), Image.Resampling.BICUBIC)


# Where the warm light sits in each scene; it drifts between them.
GLOW = ((1480, 300), (1650, 980), (960, 900), (260, 920), (1500, 760), (1600, 300), (960, 560))


def glow_at(t):
    index = next((i for i, end in enumerate(ENDS) if t < end), len(ENDS)-1)
    local = t-STARTS[index]
    a = GLOW[max(0, index-1)] if local < 1.6 and index else GLOW[index]
    b = GLOW[index]
    p = smooth(local/1.6) if index else 1
    # Still within a scene: a GIF of the film then changes only what moves.
    return a[0]+(b[0]-a[0])*p, a[1]+(b[1]-a[1])*p


def base(t):
    layer = Image.new('RGB', (W, H))
    x, y = glow_at(t)
    layer.paste(glow_sprite(), (round(x-750), round(y-750)))
    return ImageChops.add(backdrop(), layer)


def title(im, value, local, subtitle=None):
    reveal(im, 100, 150, value, 84, local, at=.1)
    if subtitle:
        reveal(im, 104, 262, subtitle, 32, local, at=.3, color=MUTED, weight='regular')


def foot(im, *lines):
    for i, value in enumerate(lines):
        txt(im, 104, 948+i*34, value, 24, MUTED, 'regular')


def badge(im, x, y, size, opacity=1.):
    if opacity <= 0:
        return
    rect(im, (x, y, x+size, y+size), mix(BG, RUBY_DEEP, opacity), radius=round(size*.26))
    glyph = lettering('R', round(size*.66), INK, 'bold')
    txt(im, x+size/2, y+(size-glyph.height)/2, 'R', round(size*.66), INK, 'bold', 'center', opacity)


def chrome(im, index, t):
    alpha = 1-smooth((t-STARTS[-1]-.2)/.5)
    if alpha > 0:
        badge(im, 104, 58, 42, alpha)
        txt(im, 160, 60, 'Rubylight', 30, INK, 'bold', opacity=alpha)
        txt(im, 1816, 70, f'{index+1:02d} / {CHAPTERS[index]}', 22, MUTED, 'mono', 'right', alpha)
    # Film progress, a hairline along the top edge.
    line(im, (0, 1, W*t/DURATION, 1), RUBY, 3)


def token(im, x, y, size=34, lit=1.):
    """The frame the film follows: a picture tile with three scanlines."""
    rect(im, (x-size/2, y-size/2, x+size/2, y+size/2), mix(CARD, RUBY, .18*lit),
         mix(DIM, RUBY, lit), max(2, round(size*.07)), radius=round(size*.2))
    for i, frac in enumerate((.56, .78, .44)):
        yy = y-size*.2+i*size*.2
        line(im, (x-size*.28, yy, x-size*.28+size*frac, yy), mix(DIM, RUBY, lit),
             max(2, round(size*.06)))


# 1 · Hook ---------------------------------------------------------------------

def hook(t):
    t += .9  # The first frame already carries the message.
    im = base(t-.9)
    reveal(im, 100, 186, 'Meet Rubylight.', 120, t, at=0)
    reveal(im, 100, 322, 'Built for Radeon.', 120, t, at=.2, color=RUBY)
    reveal(im, 106, 498, 'A Moonlight host for Windows, written in Rust.', 40, t,
           at=.55, color=INK, weight='semibold')
    reveal(im, 106, 556, 'Built around Radeon, from frame preparation to native AMD encoding.', 30, t,
           at=.7, color=MUTED, weight='regular')
    p = ease((t-1.3)/.6)
    if p:
        y = 660+30*(1-p)
        rect(im, (104, y, 1110, y+230), mix(BG, CARD, p), mix(BG, RULE, p), 2, radius=22)
        txt(im, 144, y+30, 'PICTURE AGE · GAMING PC TO LAPTOP OVER WI-FI', 22, MUTED, 'mono', opacity=p)
        txt(im, 138, y+62, f"{WIFI['mean']:.1f}", 120, RUBY, 'bold', opacity=p)
        mx = 144+width_of(f"{WIFI['mean']:.1f}", 120, 'bold')+10
        txt(im, mx, y+128, 'ms', 48, RUBY, 'semibold', opacity=p)
        txt(im, 590, y+88, '1968×2184 AV1 HDR, 120 fps', 32, INK, 'semibold', opacity=p)
        txt(im, 590, y+140, f"p95 {WIFI['p95']:.1f} ms · {WIFI['fresh']} new pictures a second", 26, MUTED, opacity=p)
    q = ease((t-1.6)/.7)
    if q:
        token(im, 1490, 470+40*(1-q), 230, q)
        txt(im, 1490, 630, 'FOLLOW THE FRAME', 22, MUTED, 'mono', 'center', ease((t-2)/.5))
    foot(im, 'RX 7900 XT host on Ethernet · rc.24 · 80 Mbps · Radeon 780M laptop on 5 GHz Wi-Fi · idle desktop',
         'October 8, 2026 · rendered picture to fully received, before decode (Moonlight adds 0.3–0.7 ms) · rust/PERFORMANCE_WORK.md')
    return im


# 2 · The compute path --------------------------------------------------------

ENCODE = (67, 133, 159)


def job(im, x0, x1, y, name, color, cursor):
    h = 54
    rect(im, (x0, y, x1, y+h), CARD, RULE, radius=6)
    p = clamp((cursor-x0)/(x1-x0))
    if p:
        rect(im, (x0, y, x0+max(6, (x1-x0)*p), y+h), mix(CARD, color, .65), radius=6)
    txt(im, (x0+x1)/2, y+13, name, 26, INK, 'bold', 'center')


def gate(im, x, y, active):
    color = MINT if active else DIM
    line(im, (x, y-13, x, y+13), color, 3)
    draw(im).polygon([(x, y-5), (x+5, y), (x, y+5), (x-5, y)], fill=color)


def queue_cursor(local):
    return 420+1280*clamp((local-.7)/6.8)


def schedule(t):
    """Scheduling schematic from the reviewed Vibepollo 2.0 path; not data."""
    im = base(t+STARTS[1])
    title(im, 'Prepare the frame alongside the game.', t,
          'GPU copies and colour conversion move to D3D12 compute.')
    cursor = queue_cursor(t)
    txt(im, 104, 356, 'OTHER SUNSHINE HOSTS · D3D11', 27, INK, 'bold')
    txt(im, 1816, 359, 'Reviewed path: Vibepollo 2.0', 26, MUTED, align='right')
    txt(im, 104, 434, 'Graphics', 26, MUTED)
    txt(im, 104, 522, 'Native AMF', 26, MUTED)
    for x0, x1 in ((420, 640), (660, 880)):
        job(im, x0, x1, 424, 'Game draw', DIM, cursor)
    job(im, 902, 1062, 424, 'Copy', RUBY, cursor)
    job(im, 1082, 1292, 424, 'RGB → YUV', RUBY, cursor)
    line(im, (1298, 451, 1320, 451, 1320, 540, 1336, 540), DIM, 2)
    gate(im, 1320, 491, cursor >= 1292)
    job(im, 1340, 1570, 512, 'Encode', ENCODE, cursor)
    arrow(im, 1578, 539, 1637, 539, MINT if cursor >= 1570 else DIM, 2)
    rect(im, (1647, 512, 1815, 566), CARD, RUBY if cursor >= 1570 else RULE, radius=6)
    txt(im, 1731, 526, 'Bitstream', 25, RUBY if cursor >= 1570 else MUTED, 'bold', 'center')
    line(im, (104, 605, 1816, 605), RULE, 1)
    txt(im, 104, 630, 'RUBYLIGHT · D3D12 COMPUTE', 27, RUBY, 'bold')
    txt(im, 104, 705, 'Graphics', 26, MUTED)
    txt(im, 104, 786, 'Compute', 26, RUBY)
    txt(im, 104, 859, 'Native AMF', 26, MUTED)
    for x0, x1 in ((420, 640), (660, 880)):
        job(im, x0, x1, 695, 'Game draw', DIM, cursor)
    job(im, 420, 580, 776, 'Copy', RUBY, cursor)
    job(im, 600, 810, 776, 'RGB → YUV', RUBY, cursor)
    gate(im, 402, 803, t >= .7)
    gate(im, 590, 803, cursor >= 580)
    line(im, (818, 803, 839, 803, 839, 876, 854, 876), DIM, 2)
    gate(im, 839, 845, cursor >= 810)
    job(im, 860, 1090, 849, 'Encode', ENCODE, cursor)
    arrow(im, 1098, 876, 1140, 876, MINT if cursor >= 1090 else DIM, 2)
    if t > 4.5:
        txt(im, 1255, 846, 'Ready for the stream.', 31, RUBY, 'bold', opacity=ease((t-4.5)/.5))
    if t > 7.2:
        txt(im, 1255, 889, 'AMF releases the surface for reuse.', 23, MUTED, opacity=ease((t-7.2)/.5))
    txt(im, 1540, 705, 'Output texture pool', 24, MUTED, align='center')
    for i in range(3):
        rect(im, (1476+i*47, 750, 1509+i*47, 777), CARD, MINT if t >= 7.2 else RULE, 2, radius=3)
    if t >= 7.2:
        p = smooth((t-7.2)/1.2)
        x = 1190+(1492-1190)*p
        y = 916+(763-916)*p-70*math.sin(p*math.pi)
        rect(im, (x-13, y-10, x+13, y+10), CARD, MINT, 2, radius=3)
    foot(im, 'Scheduling schematic · equal game and encode work · graphics and compute share GPU resources',
         'GPU textures in both paths · readiness fences protect copies and the encoder handoff')
    return im


# 3 · Radeon compute, measured -------------------------------------------------

def bars(im, x, y, width, heading_text, values, t, delay=0., unit='ms', top=60.):
    txt(im, x, y, heading_text, 34, INK, 'bold')
    for i, (value, caption) in enumerate(zip(values, ('Compute off', 'Compute on'))):
        yy = y+82+i*150
        color = RUBY if i else (126, 133, 150)
        p = ease((t-delay-i*.8)/1.1)
        txt(im, x, yy+8, caption, 28, MUTED)
        rect(im, (x, yy+58, x+width, yy+100), CARD, radius=10)
        if p:
            txt(im, x+width, yy-6, f'{value:.1f} {unit}', 52, color, 'bold', 'right', p)
            rect(im, (x, yy+58, x+max(14, width*value/top*p), yy+100), color, radius=10)


def compute(t):
    im = base(t+STARTS[3])
    title(im, 'Radeon compute: 7.5 ms sooner.', t,
          'Same Rubylight build, one setting changed, beside a game-like GPU load.')
    bars(im, 104, 360, 760, 'Average picture age', COMPUTE['mean'], t, .5)
    bars(im, 1056, 360, 760, 'Mean per-run 95th percentile', COMPUTE['p95'], t, 1.6)
    p = ease((t-3.4)/.7)
    if p:
        line(im, (104, 712, 1816, 712), mix(BG, RULE, p), 2)
        before, after = COMPUTE['host']
        txt(im, 104, 744, 'Host time · present-to-send counter', 30, MUTED, opacity=p)
        txt(im, 104, 790, f'{before:.1f} → {after:.1f} ms', 64, INK, 'bold', opacity=p)
        txt(im, 1056, 744, 'The game beside it', 30, MUTED, opacity=p)
        txt(im, 1056, 790, f"{COMPUTE['game_fps']} fps either way", 64, INK, 'bold', opacity=p)
    foot(im, 'RX 7900 XT · DDX · 1080p60 HEVC HDR · 20 Mbps · 120 Hz virtual display · game-like load',
         'October 4, 2026 · arithmetic means of two runs/path · local render-to-decode · rust/PERFORMANCE.md',
         'Host counter starts at the capture stamp, not application Present · display scanout is excluded')
    return im


def comparison(t):
    im = base(t+STARTS[2])
    title(im, 'Faster than Vibepollo, beside a game.', t,
          'Same RX 7900 XT, same settings, at 1968×2184 HDR and 120 fps.')
    txt(im, 104, 346, 'PICTURE AGE · 60 FPS GAME RUNNING', 22, MUTED, 'mono')
    txt(im, 1170, 338, 'Vibepollo 2.0', 38, INK, 'bold', 'center')
    txt(im, 1630, 338, 'Rubylight', 38, RUBY, 'bold', 'center')
    rows = (('AV1 · average', 'av1_mean'),
            ('AV1 · 95th percentile', 'av1_p95'),
            ('HEVC · average', 'hevc_mean'),
            ('HEVC · 95th percentile', 'hevc_p95'),
            ('Host latency in Moonlight · AV1', 'host'))
    for i, (label, key) in enumerate(rows):
        y = 414+i*82
        p = ease((t-.5-i*.4)/.6)
        txt(im, 104, y+10, label, 32, INK, opacity=p)
        for x, value, color in zip((1170, 1630), HOSTS[key], (INK, RUBY)):
            txt(im, x, y, f'{value:.1f} ms', 50, color, 'bold', 'center', p)
        line(im, (104, y+66, 1816, y+66), RULE)
    reveal(im, 104, 844, '7.4 ms sooner on average, 16–18 ms sooner at the 95th percentile.', 34, t,
           at=2.7, color=RUBY, weight='semibold')
    reveal(im, 104, 894, "Both use native AMF; that encoder came from Rubylight's author.", 28, t,
           at=2.9, color=MUTED, weight='regular', rise=12)
    foot(im, 'RX 7900 XT · rc.24 · Desktop Duplication · 80 Mbps · virtual HDR display · October 8, 2026',
         'Same settings: native AMF, ultra-low latency · two alternating runs per cell · 60 fps-capped game beside the stream',
         'Hardware decode by an independent client on the host, excluding network and scanout · rust/PERFORMANCE_WORK.md')
    return im


def sample_rgb(y, cb, cr):
    return tuple(round(clamp(c)*255) for c in (y+1.5748*cr, y-.1873*cb-.4681*cr, y+1.8556*cb))


def chroma_block(im, x, y, full, t):
    # Both blocks share the same luma; only the number of colour samples differs.
    lumas = (.38, .62, .62, .38)
    # Nonzero mean chroma keeps the 4:2:0 block coloured too: subsampling
    # merges local colour differences, it does not remove colour.
    chromas = ((.10, .21), (-.16, -.13), (.10, -.13), (-.16, .21))
    for i, yy in enumerate(lumas):
        xx, dy = i % 2, i//2
        cb, cr = chromas[i] if full else (-.03, .04)
        p = ease((t-.7-i*.22)/.6)
        rect(im, (x+xx*152, y+dy*152, x+xx*152+144, y+dy*152+144), sample_rgb(yy, cb*p, cr*p), radius=6)
        # A marker per colour sample, so the count reads without colour vision.
        if full:
            draw(im).ellipse((x+xx*152+65, y+dy*152+65, x+xx*152+79, y+dy*152+79), fill=BG)
    if not full:
        draw(im).ellipse((x+137, y+137, x+159, y+159), fill=RUBY, outline=BG, width=3)


def pyrowave(t):
    im = base(t+STARTS[4])
    title(im, 'Colour detail at every pixel.', t, 'PyroWave · 10-bit HDR 4:4:4 on your local network.')
    txt(im, 252, 334, '4:2:0', 34, MUTED, 'bold', 'center')
    txt(im, 708, 334, '4:4:4', 34, RUBY, 'bold', 'center')
    chroma_block(im, 104, 390, False, t)
    chroma_block(im, 560, 390, True, t)
    arrow(im, 414, 538, 546, 538, mix(RULE, RUBY, ease((t-1.7)/1.0)), 3)
    txt(im, 252, 704, '1 Cb + 1 Cr', 28, MUTED, 'regular', 'center')
    txt(im, 708, 704, '4 Cb + 4 Cr', 28, RUBY, 'bold', 'center')
    p = ease((t-1.2)/.7)
    if p:
        rect(im, (980, 390, 1816, 690), CARD, RULE, 2, radius=24)
        txt(im, 1020, 420, '1080p FRAME · ENCODE CAPACITY', 24, MUTED, 'mono', opacity=p)
        txt(im, 1010, 452, '≈0.5 ms', 120, RUBY, 'bold', opacity=p)
        txt(im, 1020, 598, 'Even beside a GPU-heavy game.', 34, INK, 'semibold', opacity=p)
        txt(im, 1020, 644, 'Repeat-frame throughput; one part of the picture journey.', 24, MUTED, opacity=p)
    reveal(im, 104, 786, 'Same pixels, four colour samples: crisp coloured text and edges.', 34, t,
           at=2.3, color=RUBY, weight='semibold', rise=12)
    reveal(im, 104, 870, 'Play it on Rubylight Android, our own client, over a fast local network.', 32, t,
           at=2.7, color=INK, weight='semibold', rise=12)
    foot(im, f'RX 7900 XT · PyroWave 1080p · 10-bit HDR 4:4:4 · 400 Mbps · game-like load · October 7, 2026 · {PYROWAVE[0]:.2f}–{PYROWAVE[1]:.2f} ms/frame',
         'Encode capacity includes conversion, not capture, transport or decode · chroma diagram is schematic · rust/PERFORMANCE.md')
    return im


def get_started(t):
    im = base(t+STARTS[5])
    title(im, 'Install. Pair. Play.', t, 'Start with Rubylight on your Windows gaming PC.')
    steps = (('Install Rubylight.', 'Run the installer and create your local account in the web console.'),
             ('Pair your device.', 'Add your PC in Rubylight Android or Moonlight, then enter its PIN in Devices.'),
             ('Launch Desktop.', 'Choose your game, then make the stream settings your own.'))
    for i, (label, detail) in enumerate(steps):
        y = 366+i*166
        p = ease((t-.4-i*.4)/.6)
        if p:
            token(im, 132, y+30, 48, p)
        txt(im, 194, y, label, 46, RUBY, 'bold', opacity=p)
        txt(im, 194, y+70, detail, 32, INK, opacity=p)
    foot(im, 'Setup can import your settings, paired devices and library from another Sunshine-based host.',
         'Walkthrough and stream-format choices: docs/getting-started.md')
    return im


# 7 · Ending -------------------------------------------------------------------

def ending(t):
    im = base(t+STARTS[6])
    p = ease((t-.1)/.7)
    mark = width_of('Rubylight', 156, 'bold')
    left = (W-(150+48+mark))/2
    badge(im, left, 222+20*(1-p), 150, p)
    reveal(im, left+198, 208, 'Rubylight', 156, t, at=.15)
    reveal(im, 960, 378, 'WINDOWS · RADEON · MOONLIGHT', 28, t, at=.35,
           color=RUBY, weight='mono', align='center')
    reveal(im, 960, 430, 'Written in Rust. Built for Radeon. Made for Moonlight.', 44, t, at=.45,
           color=INK, weight='semibold', align='center')
    words, gap = ('Install.', 'Pair.', 'Play.'), 70
    widths = [width_of(w, 62, 'bold') for w in words]
    x = (W-sum(widths)-gap*2)/2
    for i, value in enumerate(words):
        reveal(im, x, 540, value, 62, t, at=.8+i*.2, color=RUBY)
        x += widths[i]+gap
    reveal(im, 960, 720, 'github.com/RamazanKara/Rubylight', 52, t, at=1.5, weight='bold', align='center')
    txt(im, 960, 818, 'As long as AMD users are happy, Rubylight is happy.', 32, MUTED, 'regular', 'center',
        ease((t-2)/.6))
    txt(im, 960, 872, 'Formerly Butterpollo.', 26, DIM, 'regular', 'center', ease((t-2.3)/.6))
    return im


SCENES = (hook, schedule, comparison, compute, pyrowave, get_started, ending)

def scene_at(t):
    index = next((i for i, end in enumerate(ENDS) if t < end), len(ENDS)-1)
    return index, t-STARTS[index]


def frame(t):
    index, local = scene_at(t)
    im = SCENES[index](local)
    if index and local < WIPE:
        old = SCENES[index-1](ENDS[index-1]-STARTS[index-1]-.001)
        p = smooth(local/WIPE)
        lean = 260
        edge = -lean-60+(W+2*lean+120)*p
        mask = Image.new('L', (W, H), 0)
        draw(mask).polygon([(-10, 0), (edge+lean, 0), (edge, H), (-10, H)], fill=255)
        im = Image.composite(im, old, mask)
        d = draw(im)
        d.polygon([(edge+lean, 0), (edge+lean+46, 0), (edge+46, H), (edge, H)], fill=RUBY)
        d.polygon([(edge+lean+46, 0), (edge+lean+58, 0), (edge+58, H), (edge+46, H)], fill=ROSE)
    chrome(im, index, t)
    return im


def storyboard(destination):
    times = tuple(t for start, end in zip(STARTS, ENDS) for t in (start+1, end-.5))
    sheet = Image.new('RGB', (1920, 4*300), BG)
    for i, t in enumerate(times):
        tile = frame(t).resize((480, 270), Image.Resampling.LANCZOS)
        x, y = i % 4*480, i//4*300
        sheet.paste(tile, (x, y))
        txt(sheet, x+10, y+272, f'{t:04.1f}s', 20, MUTED, 'mono')
    destination.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(destination)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--output', type=Path)
    p.add_argument('--audio', type=Path)
    p.add_argument('--frame', type=float)
    p.add_argument('--storyboard', type=Path)
    p.add_argument('--fps', type=int, default=60)
    p.add_argument('--width', type=int, default=1920)
    args = p.parse_args()
    if args.storyboard:
        storyboard(args.storyboard)
    if args.output and args.frame is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        frame(args.frame).save(args.output)
    elif args.output:
        ffmpeg = shutil.which('ffmpeg')
        if not ffmpeg:
            p.error('FFmpeg is required.')
        args.output.parent.mkdir(parents=True, exist_ok=True)
        vf = f'scale={args.width}:-2:flags=lanczos:out_color_matrix=bt709:out_range=tv'
        cmd = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', '-f', 'rawvideo', '-pixel_format', 'rgb24',
               '-video_size', f'{W}x{H}', '-framerate', str(args.fps), '-i', 'pipe:0']
        if args.audio:
            cmd += ['-i', str(args.audio), '-map', '0:v', '-map', '1:a', '-c:a', 'aac', '-b:a', '192k']
        else:
            cmd += ['-an']
        cmd += ['-vf', vf, '-c:v', 'libx264', '-threads', '4', '-preset', 'medium', '-crf', '19',
                '-pix_fmt', 'yuv420p', '-movflags', '+faststart', '-color_primaries', 'bt709',
                '-color_trc', 'bt709', '-colorspace', 'bt709', '-color_range', 'tv',
                '-t', str(DURATION), str(args.output)]
        with subprocess.Popen(cmd, stdin=subprocess.PIPE) as proc:
            try:
                for n in range(DURATION*args.fps):
                    proc.stdin.write(frame(n/args.fps).tobytes())
                    if n % (args.fps*5) == 0:
                        print(f'Rendered {n//args.fps}/{DURATION}s', flush=True)
            finally:
                proc.stdin.close()
            if proc.wait():
                raise RuntimeError('FFmpeg encoding failed')
        print(f'Wrote {args.output}', flush=True)
    elif not args.storyboard:
        p.error('Use --output or --storyboard.')


if __name__ == '__main__':
    main()
