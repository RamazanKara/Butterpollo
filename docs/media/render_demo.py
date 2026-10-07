#!/usr/bin/env python3
"""Build the evergreen film, README GIF and eight-second teaser.

Requires Pillow, NumPy and FFmpeg. No service, game, display or GPU access.
Usage: python docs/media/render_demo.py --output docs/media/demo.mp4
       python docs/media/render_demo.py --storyboard storyboard.jpg
       python docs/media/render_demo.py --gif-from docs/media/demo.mp4 --output docs/media/demo.gif
"""
import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import render_launch_film as film


def encode_gif(source, destination):
    ffmpeg = shutil.which('ffmpeg')
    if not ffmpeg:
        raise RuntimeError('FFmpeg is required to encode the preview.')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='butterpollo-demo-') as directory:
        palette = str(Path(directory) / 'palette.png')
        base = [ffmpeg, '-hide_banner', '-loglevel', 'error', '-y', '-threads', '4']
        subprocess.run(base + ['-i', str(source), '-vf',
                       'fps=12,scale=960:540:flags=lanczos,palettegen=max_colors=96:stats_mode=diff',
                       '-frames:v', '1', palette], check=True)
        subprocess.run(base + ['-i', str(source), '-i', palette, '-filter_complex',
                       '[0:v]fps=12,scale=960:540:flags=lanczos[x];'
                       '[x][1:v]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle',
                       '-loop', '0', str(destination)], check=True)
    if destination.stat().st_size >= 10_000_000:
        raise RuntimeError(f'{destination} exceeds the 10 MB preview limit')
    print(f'Wrote {destination}', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--storyboard', type=Path)
    parser.add_argument('--frame', type=float)
    parser.add_argument('--gif-from', type=Path)
    parser.add_argument('--diagram', action='store_true')
    args = parser.parse_args()
    if args.storyboard:
        film.storyboard(args.storyboard)
    if args.diagram or args.frame is not None:
        if not args.output:
            parser.error('--diagram and --frame require --output.')
        args.output.parent.mkdir(parents=True, exist_ok=True)
        still = film.pipeline(9) if args.diagram else film.frame(args.frame)
        still.save(args.output)
    elif args.gif_from:
        if not args.output:
            parser.error('--gif-from requires --output.')
        encode_gif(args.gif_from, args.output)
    elif args.output:
        ffmpeg = shutil.which('ffmpeg')
        if not ffmpeg:
            parser.error('FFmpeg is required.')
        scripts = Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory(prefix='butterpollo-film-') as directory:
            score = Path(directory) / 'score.wav'
            subprocess.run([sys.executable, str(scripts / 'demo_audio.py'),
                            '--output', str(score), '--duration', str(film.DURATION)], check=True)
            subprocess.run([sys.executable, str(scripts / 'render_launch_film.py'),
                            '--output', str(args.output), '--audio', str(score)], check=True)
            encode_gif(args.output, args.output.with_suffix('.gif'))
            teaser = Path(directory) / 'teaser.mp4'
            # The introduction and invitation keep the short preview self-contained.
            subprocess.run([ffmpeg, '-hide_banner', '-loglevel', 'error', '-y',
                            '-i', str(args.output), '-filter_complex',
                            '[0:v]split[a][b];[a]trim=0:4,setpts=PTS-STARTPTS[open];'
                            '[b]trim=56:60,setpts=PTS-STARTPTS[close];'
                            '[open][close]concat=n=2:v=1:a=0[v]',
                            '-map', '[v]', '-an', '-c:v', 'libx264', '-threads', '4',
                            '-crf', '19', str(teaser)], check=True)
            encode_gif(teaser, args.output.with_name(args.output.stem + '-teaser.gif'))
    elif not args.storyboard:
        parser.error('Choose --output or --storyboard.')


if __name__ == '__main__':
    main()
