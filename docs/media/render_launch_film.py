#!/usr/bin/env python3
"""Butterpollo's 60-second launch film, rendered deterministically with Pillow.

No service, game, display or GPU access. Requires Pillow and FFmpeg.
Diagrams are schematic; every number is a measurement, named in the footer
of the scene that shows it (rust/PERFORMANCE.md has the runs).

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
BUTTER = (255, 201, 64)
CORAL = (255, 110, 78)
MINT = (98, 226, 184)
MUTED = (150, 157, 172)
DIM = (68, 74, 90)
RULE = (36, 40, 52)
CARD = (19, 22, 30)
STARTS = (0, 6, 18, 30, 37, 47, 54)
ENDS = STARTS[1:] + (60,)
CHAPTERS = ('RC.17', 'FOLLOW ONE FRAME', 'RADEON COMPUTE', 'BUILT TOGETHER',
            'MADE FOR PLAY', 'TRY IT SAFELY', 'BUTTERPOLLO')
WIPE = .7

# rc.17 with its default capture (WGC), idle desktop: October 7, 2026, three
# runs alternating with rc.2 in one batch on the October 4 fixture (RX 7900 XT,
# 1080p60 HEVC HDR, 20 Mbps, 120 Hz virtual display). Timestamped game frame
# to the decoded picture of a local client.
IDLE = {'mean': 14.86, 'p95': 16.04, 'fresh': 60.36}
# Same build, compute off and on, beside a game-like load: October 4, 2026,
# two runs per path (README and rust/PERFORMANCE.md).
COMPUTE = {'mean': (41.0, 33.5), 'p95': (54.4, 42.3), 'host': (16.2, 11.2), 'game_fps': 174}

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


def arrow(im, x0, y0, x1, y1, color=BUTTER, width=4):
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
            c = mix(CORAL, BUTTER, clamp(1.15-d))
            px[x, y] = tuple(round(v*a) for v in c)
    return im.resize((1500, 1500), Image.Resampling.BICUBIC)


# Where the warm light sits in each scene; it drifts between them.
GLOW = ((1480, 300), (1650, 980), (260, 920), (1500, 760), (300, 220), (1600, 300), (960, 560))


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
        txt(im, 104, 962+i*36, value, 24 if i == 0 else 22, MUTED if i == 0 else DIM, 'regular')


def badge(im, x, y, size, opacity=1.):
    if opacity <= 0:
        return
    rect(im, (x, y, x+size, y+size), mix(BG, BUTTER, opacity), radius=round(size*.26))
    glyph = lettering('B', round(size*.66), BG, 'bold')
    txt(im, x+size/2, y+(size-glyph.height)/2, 'B', round(size*.66), BG, 'bold', 'center', opacity)


def chrome(im, index, t):
    alpha = 1-smooth((t-54.2)/.5)
    if alpha > 0:
        badge(im, 104, 58, 42, alpha)
        txt(im, 160, 60, 'Butterpollo', 30, INK, 'bold', opacity=alpha)
        txt(im, 1816, 70, f'{index+1:02d} / {CHAPTERS[index]}', 22, MUTED, 'mono', 'right', alpha)
    # Film progress, a hairline along the top edge.
    line(im, (0, 1, W*t/DURATION, 1), BUTTER, 3)


def token(im, x, y, size=34, lit=1.):
    """The frame the film follows: a picture tile with three scanlines."""
    rect(im, (x-size/2, y-size/2, x+size/2, y+size/2), mix(CARD, BUTTER, .18*lit),
         mix(DIM, BUTTER, lit), max(2, round(size*.07)), radius=round(size*.2))
    for i, frac in enumerate((.56, .78, .44)):
        yy = y-size*.2+i*size*.2
        line(im, (x-size*.28, yy, x-size*.28+size*frac, yy), mix(DIM, BUTTER, lit),
             max(2, round(size*.06)))


# 1 · Hook ---------------------------------------------------------------------

def hook(t):
    t += .9  # The first frame already carries the message.
    im = base(t-.9)
    reveal(im, 100, 186, 'Your Radeon was', 120, t, at=0)
    reveal(im, 100, 322, 'never the problem.', 120, t, at=.2, color=BUTTER)
    reveal(im, 106, 498, 'Written in Rust. Built for Radeon. Made for Moonlight.', 40, t,
           at=.55, color=INK, weight='semibold')
    reveal(im, 106, 556, 'Butterpollo rc.17 · Windows game streaming for Radeon owners', 30, t,
           at=.7, color=MUTED, weight='regular')
    p = ease((t-1.3)/.6)
    if p:
        y = 660+30*(1-p)
        rect(im, (104, y, 1110, y+230), mix(BG, CARD, p), mix(BG, RULE, p), 2, radius=22)
        txt(im, 144, y+30, 'GAME FRAME → DECODED PICTURE', 22, MUTED, 'mono', opacity=p)
        txt(im, 138, y+62, f"{IDLE['mean']:.1f}", 120, BUTTER, 'bold', opacity=p)
        mx = 144+width_of(f"{IDLE['mean']:.1f}", 120, 'bold')+10
        txt(im, mx, y+128, 'ms', 48, BUTTER, 'semibold', opacity=p)
        txt(im, 600, y+76, f"{IDLE['p95']:.1f} ms", 44, INK, 'bold', opacity=p)
        txt(im, 600, y+128, 'for the slowest 5%', 26, MUTED, opacity=p)
        txt(im, 850, y+76, f"{IDLE['fresh']:.0f}", 44, INK, 'bold', opacity=p)
        txt(im, 850, y+128, 'new pictures / s', 26, MUTED, opacity=p)
    q = ease((t-1.6)/.7)
    if q:
        token(im, 1490, 470+40*(1-q), 230, q)
        txt(im, 1490, 630, 'FOLLOW THE FRAME', 22, MUTED, 'mono', 'center', ease((t-2)/.5))
    foot(im, 'Average · rc.17 default capture (WGC) · idle desktop · RX 7900 XT · 1080p60 HEVC HDR · 20 Mbps',
         'October 7, 2026 · timestamped game frame to the decoded picture of a local Moonlight client · three runs')
    return im


# 2 · Follow one frame ---------------------------------------------------------

STATIONS = (('Game', 'renders on the graphics queue', 'monitor'),
            ('Capture', 'WGC wakes the host as the frame lands', 'capture'),
            ('Convert', 'Radeon compute, beside the game', 'convert'),
            ('Encode', 'native AMF, not a generic wrapper', 'encode'),
            ('Send', 'paced, and tagged for Wi-Fi priority', 'send'),
            ('Decode', 'Moonlight shows the picture', 'decode'))
CARD_W, GAP, CARD_Y = 262, 28, 430


def station_x(i):
    return 104+i*(CARD_W+GAP)


def arrive(i):
    return 1.1+i*1.2


def icon(im, kind, cx, cy, color):
    d = draw(im)
    if kind == 'monitor':
        d.rounded_rectangle((cx-38, cy-26, cx+38, cy+20), radius=5, outline=color, width=4)
        line(im, (cx, cy+20, cx, cy+32), color, 4)
        line(im, (cx-18, cy+33, cx+18, cy+33), color, 4)
    elif kind == 'capture':
        for sx, sy in ((-1, -1), (1, -1), (-1, 1), (1, 1)):
            x, y = cx+sx*34, cy+sy*24
            line(im, (x, y, x-sx*16, y), color, 4)
            line(im, (x, y, x, y-sy*14), color, 4)
        d.ellipse((cx-9, cy-9, cx+9, cy+9), fill=color)
    elif kind == 'convert':
        for i, c in enumerate(((235, 90, 80), (90, 210, 120), (90, 140, 245))):
            d.ellipse((cx-44+i*15, cy-22, cx-24+i*15, cy-2), fill=mix(DIM, c, color == BUTTER))
        arrow(im, cx-2, cy-12, cx+16, cy-12, color, 3)
        d.rounded_rectangle((cx+24, cy-28, cx+44, cy+4), radius=3, outline=color, width=3)
        line(im, (cx-40, cy+18, cx+44, cy+18), color, 4)
    elif kind == 'encode':
        for i, w in enumerate((76, 56, 36, 20)):
            d.rounded_rectangle((cx-w/2, cy-28+i*15, cx+w/2, cy-20+i*15), radius=3, fill=color)
    elif kind == 'send':
        for r in (14, 30, 46):
            d.arc((cx-r, cy+18-r, cx+r, cy+18+r), 225, 315, fill=color, width=5)
        d.ellipse((cx-6, cy+12, cx+6, cy+24), fill=color)
    else:
        d.rounded_rectangle((cx-38, cy-26, cx+38, cy+26), radius=7, outline=color, width=4)
        d.polygon([(cx-10, cy-14), (cx-10, cy+14), (cx+16, cy)], fill=color)


def pipeline(t):
    im = base(t+6)
    title(im, 'Follow one frame.', t, 'The Radeon path from your game to the picture in Moonlight.')
    rail_y = 386
    first, last = station_x(0)+CARD_W/2, station_x(5)+CARD_W/2
    line(im, (first, rail_y, last, rail_y), RULE, 4)
    travel = clamp((t-arrive(0))/(arrive(5)-arrive(0)))
    tx = first+(last-first)*smooth(travel) if t >= arrive(0) else first
    if t >= arrive(0):
        line(im, (first, rail_y, tx, rail_y), BUTTER, 4)
    for i, (name, caption, kind) in enumerate(STATIONS):
        x = station_x(i)
        shown = ease((t-.35-i*.12)/.5)
        if shown <= 0:
            continue
        y = CARD_Y+24*(1-shown)
        lit = smooth((t-arrive(i)+.15)/.35)
        rect(im, (x, y, x+CARD_W, y+262), mix(BG, CARD, shown), mix(RULE, BUTTER, lit*.9), 2, radius=18)
        draw(im).ellipse((x+CARD_W/2-7, rail_y-7, x+CARD_W/2+7, rail_y+7), fill=mix(DIM, BUTTER, lit))
        icon(im, kind, x+CARD_W/2, y+68, mix(DIM, BUTTER, lit))
        txt(im, x+26, y+122, name, 34, mix(MUTED, INK, max(lit, .35)), 'bold', opacity=shown)
        for j, value in enumerate(wrap(caption, 23, CARD_W-50)):
            txt(im, x+26, y+172+j*30, value, 23, MUTED, opacity=shown)
    if t < arrive(5)+.6:
        token(im, tx, rail_y, 46, 1)
    p = ease((t-arrive(5)-.4)/.7)
    if p:
        y = 748+26*(1-p)
        txt(im, 104, y, 'THE WHOLE TRIP, MEASURED', 22, MUTED, 'mono', opacity=p)
        txt(im, 98, y+28, f"{IDLE['mean']:.1f} ms", 104, BUTTER, 'bold', opacity=p)
        right = 104+width_of(f"{IDLE['mean']:.1f} ms", 104, 'bold')+56
        txt(im, right, y+50, f"average · {IDLE['p95']:.1f} ms for the slowest 5%", 34, INK, 'semibold', opacity=p)
        txt(im, right, y+100, f"{IDLE['fresh']:.0f} new pictures every second, none repeated", 30, MUTED, opacity=p)
    foot(im, 'Schematic order of work · measured: rc.17 default capture (WGC), idle desktop, 1080p60 HEVC HDR, 20 Mbps',
         'RX 7900 XT · October 7, 2026 · timestamped game frame to a local client\u2019s decoded picture · three runs')
    return im


# 3 · Radeon compute, measured -------------------------------------------------

def bars(im, x, y, width, heading_text, values, t, delay=0., unit='ms', top=60.):
    txt(im, x, y, heading_text, 34, INK, 'bold')
    for i, (value, caption) in enumerate(zip(values, ('Compute off', 'Compute on'))):
        yy = y+82+i*150
        color = BUTTER if i else (126, 133, 150)
        p = ease((t-delay-i*.8)/1.1)
        txt(im, x, yy+8, caption, 28, MUTED)
        rect(im, (x, yy+58, x+width, yy+100), CARD, radius=10)
        if p:
            txt(im, x+width, yy-6, f'{value:.1f} {unit}', 52, color, 'bold', 'right', p)
            rect(im, (x, yy+58, x+max(14, width*value/top*p), yy+100), color, radius=10)


def compute(t):
    im = base(t+18)
    title(im, 'Radeon compute, measured.', t,
          'Same Butterpollo build, one setting changed, a game loading the GPU.')
    bars(im, 104, 360, 760, 'Average delay', COMPUTE['mean'], t, .5)
    bars(im, 1056, 360, 760, 'Slowest 5%', COMPUTE['p95'], t, 1.6)
    p = ease((t-3.4)/.7)
    if p:
        line(im, (104, 712, 1816, 712), mix(BG, RULE, p), 2)
        before, after = COMPUTE['host']
        txt(im, 104, 744, 'Host time, present to send', 30, MUTED, opacity=p)
        txt(im, 104, 790, f'{before:.1f} → {after:.1f} ms', 64, INK, 'bold', opacity=p)
        txt(im, 1056, 744, 'The game beside it', 30, MUTED, opacity=p)
        txt(im, 1056, 790, f"{COMPUTE['game_fps']} fps either way", 64, INK, 'bold', opacity=p)
    foot(im, 'Copies and colour conversion on D3D12 compute queues instead of the graphics queue the game uses',
         'RX 7900 XT · DDX · 1080p60 HEVC HDR · 120 Hz virtual display · two runs per path · October 4, 2026 · rust/PERFORMANCE.md')
    return im


# 4 · Built together -----------------------------------------------------------

def together(t):
    im = base(t+30)
    title(im, 'Built on good work.', t,
          'Butterpollo began as a fork of Vibepollo and rebuilds the host in Rust around the Radeon path.')
    names = (('Vibepollo', 'Nonary'), ('Apollo', 'ClassicOldSong'), ('Sunshine', 'LizardByte and contributors'),
             ('PyroWave', 'Themaister · joemossjr16'))
    for i, (name, who) in enumerate(names):
        p = ease((t-.5-i*.2)/.6)
        x = 104+i*434
        rect(im, (x, 400+20*(1-p), x+404, 540+20*(1-p)), mix(BG, CARD, p), mix(BG, RULE, p), 2, radius=20)
        txt(im, x+30, 426+20*(1-p), name, 38, INK, 'bold', opacity=p)
        txt(im, x+30, 482+20*(1-p), who, 24, MUTED, opacity=p)
    reveal(im, 104, 616, 'Anything that works out here is GPL-3.0 for Vibepollo to take.', 40, t,
           at=1.7, color=INK, weight='semibold')
    p = ease((t-2.6)/.6)
    if p:
        label = 'On NVIDIA? Use Vibepollo.'
        glyph = lettering(label, 40, BUTTER, 'bold')
        y = 712+16*(1-p)
        rect(im, (104, y, 104+glyph.width+96, y+96), mix(BG, CARD, p), mix(BG, BUTTER, p*.8), 2, radius=48)
        txt(im, 152, y+(96-glyph.height)/2, label, 40, BUTTER, 'bold', opacity=p)
    foot(im, 'Butterpollo includes NVENC, but it has not been tested on NVIDIA hardware',
         'The native AMF encoder both hosts use came from Butterpollo\u2019s author (Vibepollo #342)')
    return im


# 5 · Made for play ------------------------------------------------------------

FEATURES = (('Radeon quirks', 'Driver bugs worked around, and the fix removed once AMD ships one.', 'chip'),
            ('Steam Deck', 'Gyro and touchpad work: it becomes a virtual DualSense.', 'deck'),
            ('Wi-Fi', 'Video and voice go first on your home network.', 'wifi'),
            ('HDR', '5,173 of 5,173 frames decoded in native HDR tests.', 'hdr'),
            ('PyroWave', '10-bit HDR 4:4:4: full colour at every pixel.', 'pixels'),
            ('VRR', 'Nonary\u2019s 1000 Hz mode: frames follow your game.', 'vrr'))


def feature_icon(im, kind, x, y, color):
    d = draw(im)
    if kind == 'chip':
        d.rounded_rectangle((x+14, y+6, x+70, y+54), radius=6, outline=color, width=4)
        d.rounded_rectangle((x+30, y+20, x+54, y+40), radius=3, fill=color)
        for i in range(3):
            line(im, (x+2, y+16+i*14, x+14, y+16+i*14), color, 3)
            line(im, (x+70, y+16+i*14, x+82, y+16+i*14), color, 3)
    elif kind == 'deck':
        d.rounded_rectangle((x, y+8, x+92, y+50), radius=16, outline=color, width=4)
        d.rounded_rectangle((x+28, y+16, x+64, y+42), radius=3, outline=color, width=3)
        d.ellipse((x+8, y+22, x+20, y+34), fill=color)
        d.ellipse((x+72, y+22, x+84, y+34), fill=color)
    elif kind == 'wifi':
        for r in (12, 26, 40):
            d.arc((x+46-r, y+50-r, x+46+r, y+50+r), 220, 320, fill=color, width=5)
        d.ellipse((x+40, y+44, x+52, y+56), fill=color)
    elif kind == 'hdr':
        for i in range(6):
            d.rectangle((x+i*15, y+10, x+i*15+13, y+50), fill=mix((40, 44, 56), color, i/5))
    elif kind == 'pixels':
        for i in range(4):
            for j in range(3):
                d.rectangle((x+i*22, y+6+j*16, x+i*22+18, y+18+j*16),
                            fill=mix((40, 44, 56), color, ((i+j) % 3+1)/3))
    else:
        points = [(x+i*9, y+30-18*math.sin(i*.8)*math.exp(-i*.04)) for i in range(11)]
        d.line(points, fill=color, width=4, joint='curve')


def play(t):
    im = base(t+37)
    title(im, 'Made for how you play.', t)
    cw, ch, gx, gy = 548, 250, 34, 30
    for i, (name, text, kind) in enumerate(FEATURES):
        col, row = i % 3, i//3
        x, y = 104+col*(cw+gx), 330+row*(ch+gy)
        p = ease((t-.4-i*.3)/.6)
        if p <= 0:
            continue
        y += 34*(1-p)
        rect(im, (x, y, x+cw, y+ch), mix(BG, CARD, p), mix(BG, RULE, p), 2, radius=20)
        feature_icon(im, kind, x+34, y+30, mix(BG, BUTTER, p))
        txt(im, x+34, y+102, name, 38, INK, 'bold', opacity=p)
        for j, value in enumerate(wrap(text, 26, cw-68)):
            txt(im, x+34, y+156+j*34, value, 26, MUTED, opacity=p)
    foot(im, 'HDR: rc.10 native HEVC and AV1 runs, decoded BT.2020 / PQ checked against reference colours',
         'PyroWave and the 1000 Hz VRR mode need Nonary\u2019s Moonlight client; PyroWave wants a fast wired network')
    return im


# 6 · Try it safely ------------------------------------------------------------

def safely(t):
    im = base(t+47)
    title(im, 'Trying it costs nothing.', t, 'Already streaming with another host? Setup brings your profile along.')
    for i, name in enumerate(('Sunshine', 'Apollo', 'Vibeshine', 'Vibepollo')):
        p = ease((t-.4-i*.15)/.5)
        x = 104+i*250
        rect(im, (x, 380, x+226, 446), mix(BG, CARD, p), mix(BG, RULE, p), 2, radius=33)
        txt(im, x+113, 392, name, 30, INK, 'semibold', 'center', p)
    points = ('Settings, paired devices, apps and covers come along.',
              'The original profile stays untouched.',
              'If the import fails, your old host keeps running.',
              'An interrupted update rolls itself back.')
    for i, value in enumerate(points):
        p = ease((t-1.2-i*.35)/.6)
        y = 518+i*86
        if p:
            draw(im).ellipse((104, y+4, 144, y+44), fill=mix(BG, MINT, p))
            line(im, (114, y+25, 122, y+33), BG, 5)
            line(im, (122, y+33, 136, y+15), BG, 5)
        txt(im, 172, y, value, 36, INK, 'semibold', opacity=p)
    foot(im, 'Setup checks the imported profile with Butterpollo\u2019s own loaders before it switches',
         'Uninstalling removes only Butterpollo\u2019s own files')
    return im


# 7 · Ending -------------------------------------------------------------------

def ending(t):
    im = base(t+54)
    p = ease((t-.1)/.7)
    mark = width_of('Butterpollo', 156, 'bold')
    left = (W-(150+48+mark))/2
    badge(im, left, 222+20*(1-p), 150, p)
    reveal(im, left+198, 208, 'Butterpollo', 156, t, at=.15)
    reveal(im, 960, 430, 'Written in Rust. Built for Radeon. Made for Moonlight.', 44, t, at=.45,
           color=INK, weight='semibold', align='center')
    words, gap = ('Install.', 'Pair.', 'Play.'), 70
    widths = [width_of(w, 62, 'bold') for w in words]
    x = (W-sum(widths)-gap*2)/2
    for i, value in enumerate(words):
        reveal(im, x, 540, value, 62, t, at=.8+i*.2, color=BUTTER)
        x += widths[i]+gap
    reveal(im, 960, 720, 'github.com/RamazanKara/Butterpollo', 52, t, at=1.5, weight='bold', align='center')
    txt(im, 960, 818, 'As long as AMD users are happy, Butterpollo is happy.', 32, MUTED, 'regular', 'center',
        ease((t-2)/.6))
    return im


SCENES = (hook, pipeline, compute, together, play, safely, ending)

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
        d.polygon([(edge+lean, 0), (edge+lean+46, 0), (edge+46, H), (edge, H)], fill=BUTTER)
        d.polygon([(edge+lean+46, 0), (edge+lean+58, 0), (edge+58, H), (edge+46, H)], fill=CORAL)
    chrome(im, index, t)
    return im


def storyboard(destination):
    times = (.5, 3.5, 7.5, 11, 14.5, 20, 24, 28.5, 33, 36, 40, 45, 49, 53, 56, 59.5)
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
