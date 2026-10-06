#!/usr/bin/env python3
"""Original, deterministic 100 BPM instrumental bed for the launch film.

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
SEED = 20261006
TRANSITIONS = [0, 5, 16, 26, 38, 46, 54, 60]


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


def render(duration: float) -> tuple[np.ndarray, dict]:
    rng = np.random.default_rng(SEED)
    count = round(duration * RATE)
    scale = duration / 60
    pads = np.zeros((count, 2), dtype=np.float32)
    pulses = np.zeros_like(pads)
    rhythm = np.zeros_like(pads)
    bass = np.zeros_like(pads)
    # D minor 9 -> Bb major 9 -> F major 9 -> C suspended -> D minor 9.
    # Voicings stay in the warm middle register, with no bright lead melody.
    chords = [([50, 57, 60, 65, 69], 38), ([46, 53, 57, 60, 65], 34),
              ([48, 53, 57, 64, 67], 41), ([48, 55, 62, 65, 69], 36),
              ([50, 57, 60, 64, 69], 38)]
    changes = [0, 16, 26, 38, 46, 60]
    for index, (notes, _) in enumerate(chords):
        begin = max(0, changes[index] * scale - 1.1)
        end = min(duration, changes[index + 1] * scale + 1.1)
        for voice, note in enumerate(notes):
            pan = (voice - 2) * .23
            for detune in [-.028, .028]:
                sound = tone(note + detune, end - begin, rng, "pad")
                add(pads, sound * .033, begin, pan + detune * 2)

    def chord_at(seconds: float) -> tuple[list[int], int]:
        position = min(4, max(0, int(np.searchsorted(changes, seconds / scale) - 1)))
        return chords[position]

    beat = .6  # 100 BPM regardless of requested film duration.
    for step in range(math.ceil(duration / (beat / 2))):
        at = step * beat / 2
        film_time = at / scale
        if not 5 <= film_time < 55:
            continue
        notes, root = chord_at(at)
        density = 2 if film_time < 16 or 38 <= film_time < 46 else 1
        if step % density == 0:
            sequence = [0, 2, 1, 3, 2, 4, 1, 2]
            note = notes[sequence[step % 8]]
            level = .072 if 16 <= film_time < 38 or 46 <= film_time < 54 else .047
            level *= 1 + rng.uniform(-.07, .07)
            add(pulses, tone(note, .92, rng) * level,
                at + (.009 if step % 2 else 0), .25 * math.sin(step * .47))
        if step % 4 == 0 and film_time < 54:
            level = .063 if film_time >= 16 else .035
            if 38 <= film_time < 46:
                level *= .65
            add(bass, tone(root, 1.05, rng, "bass") * level, at)
        if step % 2 == 0 and film_time < 54 and not 38 <= film_time < 42:
            t = np.arange(round(.35 * RATE), dtype=np.float32) / RATE
            phase = 2 * math.pi * (47 * t + 2.3 * (1 - np.exp(-t / .028)))
            kick = np.sin(phase) * smooth(t / .009) * np.exp(-t / .105)
            kick *= smooth((.35 - t) / .075)
            accent = .095 if step % 8 == 0 else .068
            add(rhythm, kick * accent * (1 if film_time >= 16 else .72), at)
            # Mild 110 ms bass duck makes room without audible pumping.
            first = round(at * RATE)
            length = min(round(.11 * RATE), count - first)
            duck = .8 + .2 * smooth(np.arange(length) / max(1, length - 1))
            bass[first:first + length] *= duck[:, None]

    # Dark stereo ambience, using only delayed versions of our own synthesis.
    send = pads * .2 + pulses * .48
    ambience = np.zeros_like(send)
    for index, delay in enumerate([.071, .113, .173, .239, .313, .419, .541,
                                   .677, .839, 1.013, 1.213, 1.447, 1.709]):
        offset = round(delay * RATE)
        gain = .19 * math.exp(-delay / .68)
        source = send[:, ::-1] if index % 2 else send
        ambience[offset:] += source[:-offset] * gain
    ambience = lowpass(ambience, 1900)
    mix = pads + pulses + bass + rhythm + ambience
    timeline = np.arange(count, dtype=np.float32) / RATE
    mix *= (smooth(timeline / (2.4 * scale))
            * smooth((duration - .35 - timeline) / (3.9 * scale)))[:, None]
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
        "pcm_bits": 24, "duration_seconds": count / RATE, "bpm": 100,
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
