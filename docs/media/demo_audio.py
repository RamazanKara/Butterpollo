#!/usr/bin/env python3
"""Original, deterministic 112 BPM instrumental bed for the launch film.

Only NumPy and Python's standard library are required. No recorded samples,
external assets, soundfonts, or network requests are used. Outputs 48 kHz stereo
24-bit PCM plus an adjacent JSON measurement report. The -20 LUFS target is an
integrated BS.1770 K-weighted estimate, not a claim about perceived music quality.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import wave

import numpy as np


RATE = 48_000
SEED = 20261019
TRANSITIONS = [0, 6, 17, 26, 33, 38, 47, 54, 60]


def smooth(value: np.ndarray) -> np.ndarray:
    value = np.clip(value, 0, 1)
    return value * value * (3 - 2 * value)


def hz(midi: float) -> float:
    return 440 * 2 ** ((midi - 69) / 12)


def add(stem: np.ndarray, sound: np.ndarray, at: float, pan: float = 0) -> None:
    start = round(at * RATE)
    skip = max(0, -start)
    start = max(0, start)
    count = min(len(sound) - skip, len(stem) - start)
    if count <= 0:
        return
    angle = (pan + 1) * math.pi / 4
    block = sound[skip : skip + count]
    stem[start : start + count, 0] += block * math.cos(angle)
    stem[start : start + count, 1] += block * math.sin(angle)


def tone(midi: float, seconds: float, rng: np.random.Generator,
         kind: str = "pluck") -> np.ndarray:
    t = np.arange(round(seconds * RATE), dtype=np.float32) / RATE
    frequency = hz(midi)
    if kind == "pad":
        # Gently detuned partials, dark upper harmonics, no oscillator resets
        # inside a chord. Long attack/release keeps chord boundaries smooth.
        phase = rng.uniform(0, 2 * math.pi)
        vibrato = .014 * np.sin(2 * math.pi * .11 * t + phase)
        sound = np.zeros_like(t)
        for partial in range(1, 5):
            strength = partial ** -2.25
            angle = 2 * math.pi * frequency * partial * t
            sound += strength * np.sin(angle + phase + partial * vibrato)
        envelope = smooth(t / 1.7) * smooth((seconds - t) / 1.7)
        envelope *= .91 + .09 * np.sin(2 * math.pi * .07 * t + phase)
    elif kind == "bass":
        sound = np.sin(2 * math.pi * frequency * t)
        sound += .12 * np.sin(4 * math.pi * frequency * t)
        envelope = smooth(t / .065) * smooth((seconds - t) / .18)
        envelope *= np.exp(-t / 1.3)
    else:
        sound = np.zeros_like(t)
        for partial in range(1, 6):
            # Higher harmonics decay first, like a low-passed analog pluck.
            strength = partial ** -1.9 * math.exp(-frequency * partial / 2200)
            sound += strength * np.sin(2 * math.pi * frequency * partial * t)
        envelope = smooth(t / .028) * np.exp(-t / .24)
        envelope *= smooth((seconds - t) / .12)
    return (sound * envelope).astype(np.float32)


def lowpass(audio: np.ndarray, cutoff: float) -> np.ndarray:
    """Gentle, zero-phase lowpass for an offline ambience stem only."""
    size = 1 << (len(audio) + 8191).bit_length()
    frequency = np.fft.rfftfreq(size, 1 / RATE)
    response = 1 / np.sqrt(1 + (frequency / cutoff) ** 4)
    filtered = np.fft.irfft(
        np.fft.rfft(audio, size, axis=0) * response[:, None], size, axis=0
    )[:len(audio)]
    return filtered.astype(np.float32)


def impulse(b: list[float], a: list[float], count: int = 8192) -> np.ndarray:
    """Biquad impulse response; short recurrence avoids any extra DSP package."""
    values = np.zeros(count, dtype=np.float64)
    for i in range(count):
        values[i] = (b[i] if i < len(b) else 0)
        if i:
            values[i] -= a[1] * values[i - 1]
        if i > 1:
            values[i] -= a[2] * values[i - 2]
    return values


def integrated_lufs(audio: np.ndarray) -> float:
    # Standard 48 kHz K-weighting biquads, 400 ms blocks with 75% overlap,
    # absolute -70 LUFS and relative -10 LU gates; stereo channel weights 1,1.
    shelf = impulse([1.53512485958697, -2.69169618940638, 1.19839281085285],
                    [1, -1.69065929318241, .73248077421585])
    highpass = impulse([1, -2, 1], [1, -1.99004745483398, .99007225036621])
    size = 1 << (len(audio) + len(shelf) + len(highpass) - 2).bit_length()
    response = np.fft.rfft(shelf, size) * np.fft.rfft(highpass, size)
    weighted = np.fft.irfft(
        np.fft.rfft(audio, size, axis=0) * response[:, None], size, axis=0
    )[:len(audio)]
    cumulative = np.r_[0., np.cumsum(np.sum(weighted * weighted, axis=1))]
    block, step = round(.4 * RATE), round(.1 * RATE)
    starts = np.arange(0, len(audio) - block + 1, step)
    energies = (cumulative[starts + block] - cumulative[starts]) / block
    energies = energies[energies > 10 ** ((-70 + .691) / 10)]
    if not len(energies):
        return -math.inf
    energies = energies[energies >= np.mean(energies) * .1]
    return -.691 + 10 * math.log10(float(np.mean(energies)))


def noise(seconds: float, rng: np.random.Generator, decay: float, bright: int) -> np.ndarray:
    """A short noise burst; each difference pass tilts it brighter."""
    t = np.arange(round(seconds * RATE), dtype=np.float32) / RATE
    sound = rng.standard_normal(len(t)).astype(np.float32)
    for _ in range(bright):
        sound = np.diff(sound, prepend=0).astype(np.float32)
    sound /= max(1e-9, float(np.max(np.abs(sound))))
    return sound * smooth(t / .002) * np.exp(-t / decay) * smooth((seconds - t) / .01)


def render(duration: float) -> tuple[np.ndarray, dict]:
    rng = np.random.default_rng(SEED)
    count = round(duration * RATE)
    scale = duration / 60
    pads = np.zeros((count, 2), dtype=np.float32)
    pulses = np.zeros_like(pads)
    rhythm = np.zeros_like(pads)
    bass = np.zeros_like(pads)
    air = np.zeros_like(pads)
    # One chord per scene: A minor 9, F major 9, C add 9, E minor 9 for the
    # new measurements, G suspended, A minor 9, D minor 9, then C major 9.
    chords = [([45, 52, 55, 59, 64], 33), ([41, 48, 52, 55, 60], 29),
              ([48, 55, 60, 62, 64], 36), ([52, 55, 59, 62, 66], 28),
              ([43, 50, 55, 57, 62], 31), ([45, 52, 55, 59, 64], 33),
              ([50, 53, 57, 60, 64], 38), ([48, 55, 59, 62, 67], 36)]
    changes = TRANSITIONS
    for index, (notes, _) in enumerate(chords):
        begin = max(0, changes[index] * scale - .9)
        end = min(duration, changes[index + 1] * scale + .9)
        for voice, note in enumerate(notes):
            pan = (voice - 2) * .25
            for detune in [-.03, .03]:
                sound = tone(note + detune, end - begin, rng, "pad")
                add(pads, sound * .028, begin, pan + detune * 2)

    def scene(seconds: float) -> int:
        return min(7, max(0, int(np.searchsorted(changes, seconds / scale, side="right") - 1)))

    beat = 60 / 112  # 112 BPM regardless of the requested film duration.
    eighth = beat / 2
    for step in range(math.ceil(duration / eighth)):
        at = step * eighth
        film_time = at / scale
        part = scene(at)
        notes, root = chords[part]
        if film_time >= 59:
            continue
        # Arpeggio: sparse in the opening and the comparison, on every
        # eighth where the film moves fastest.
        every = {0: 4, 4: 2, 7: 4}.get(part, 1)
        if step % every == 0 and film_time < 57:
            order = [0, 2, 4, 3, 1, 3, 2, 4]
            note = notes[order[step % 8]] + (12 if part in (2, 3, 5) and step % 8 == 6 else 0)
            level = (.05 if part in (0, 7) else .068) * (1 + rng.uniform(-.06, .06))
            add(pulses, tone(note, .7, rng) * level, at + (.007 if step % 2 else 0),
                .3 * math.sin(step * .53))
        if 6 <= film_time < 54:
            if step % 2 == 0 and (part != 4 or step % 4 == 0):
                t = np.arange(round(.32 * RATE), dtype=np.float32) / RATE
                phase = 2 * math.pi * (49 * t + 2.6 * (1 - np.exp(-t / .025)))
                kick = np.sin(phase) * smooth(t / .006) * np.exp(-t / .09) * smooth((.32 - t) / .06)
                add(rhythm, kick * (.1 if step % 8 == 0 else .08), at)
                first = round(at * RATE)
                length = min(round(.1 * RATE), count - first)
                duck = .78 + .22 * smooth(np.arange(length) / max(1, length - 1))
                bass[first:first + length] *= duck[:, None]
            if step % 4 == 0 and part != 4:
                add(bass, tone(root, beat * 1.9, rng, "bass") * .07, at)
            if film_time >= 17 and step % 2 == 1 and part != 4:
                add(air, noise(.06, rng, .018, 2) * .03 * (1 + rng.uniform(-.1, .1)), at,
                    .35 if step % 4 == 1 else -.35)
            if part in (2, 3, 5, 6) and step % 4 == 2:
                add(air, noise(.22, rng, .06, 1) * .05, at, .08)
    # A soft swish under each scene change, timed with the diagonal wipe.
    for change in changes[1:-1]:
        t = np.arange(round(.9 * RATE), dtype=np.float32) / RATE
        swish = noise(.9, rng, 10, 1) * smooth(t / .45) * smooth((.9 - t) / .4)
        add(air, lowpass(np.stack([swish, swish[::-1]], 1), 3200)[:, 0] * .05,
            change * scale - .35, 0)
    # A last bell on the ending chord.
    for i, note in enumerate([60, 64, 67, 71]):
        add(pulses, tone(note + 12, 3.2, rng) * .05, 54.15 * scale + i * .09, (i - 1.5) * .25)

    send = pads * .22 + pulses * .5
    ambience = np.zeros_like(send)
    for index, delay in enumerate([.067, .109, .167, .229, .307, .401, .523,
                                   .661, .821, .997, 1.193, 1.423]):
        offset = round(delay * RATE)
        gain = .18 * math.exp(-delay / .62)
        source = send[:, ::-1] if index % 2 else send
        ambience[offset:] += source[:-offset] * gain
    ambience = lowpass(ambience, 2400)
    mix = pads + pulses + bass + rhythm + air + ambience
    timeline = np.arange(count, dtype=np.float32) / RATE
    mix *= (smooth(timeline / (1.2 * scale))
            * smooth((duration - .35 - timeline) / (3.6 * scale)))[:, None]
    # Fade, then a truly silent 350 ms tail (including PCM quantization).
    tail = max(1, round(.35 * RATE))
    mix[-tail:] = 0
    mix -= np.mean(mix, axis=0, keepdims=True) * smooth(
        timeline / 1.5)[:, None] * smooth((duration - .35 - timeline) / 1.5)[:, None]
    assert np.isfinite(mix).all()
    initial = integrated_lufs(mix)
    gain = 10 ** ((-20 - initial) / 20)
    peak = float(np.max(np.abs(mix)))
    gain = min(gain, 10 ** (-3 / 20) / max(peak, 1e-12))
    mix *= gain
    report = {
        "original_synthesis": True, "copyrighted_or_recorded_samples": False,
        "seed": SEED, "sample_rate": RATE, "channels": 2,
        "pcm_bits": 24, "duration_seconds": count / RATE, "bpm": 112,
        "transition_seconds": [v * scale for v in TRANSITIONS],
        "integrated_lufs_estimate": initial + 20 * math.log10(gain),
        "sample_peak_dbfs": 20 * math.log10(float(np.max(np.abs(mix)))),
        "rms_dbfs": 20 * math.log10(float(np.sqrt(np.mean(mix * mix)))),
        "stereo_correlation": float(np.corrcoef(mix.T)[0, 1]),
        "silent_tail_seconds": tail / RATE,
        "quality_limit": "Procedural original; listening review remains required.",
        "measurement": "48 kHz K weighting; 400 ms/100 ms stereo blocks; -70 LUFS absolute and -10 LU relative gates. Sample peak is not true peak.",
    }
    return mix, report

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--duration", type=float, default=60)
    args = parser.parse_args()
    if not 10 <= args.duration <= 180:
        parser.error("duration must be between 10 and 180 seconds")
    audio, report = render(args.duration)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    quantized = np.rint(np.clip(audio, -1, 1) * 8_388_607).astype("<i4")
    packed = quantized.reshape(-1).view(np.uint8).reshape(-1, 4)[:, :3].tobytes()
    with wave.open(str(args.output), "wb") as output:
        output.setparams((2, 3, RATE, len(audio), "NONE", "not compressed"))
        output.writeframes(packed)
    with wave.open(str(args.output), "rb") as checked:
        assert checked.getparams()[:4] == (2, 3, RATE, len(audio))
        checked.setpos(len(audio) - round(.35 * RATE))
        assert not any(checked.readframes(round(.35 * RATE)))
    report["file_bytes"] = args.output.stat().st_size
    report["wav_format_and_silent_tail_verified"] = True
    args.output.with_suffix(".json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
