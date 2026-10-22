"""Generate an identifiable PCM loop and inspect captures without third-party packages."""

import array
import math
import random
import sys
import wave

RATE = 48_000
SIGNAL_SECONDS = 31
SIGNAL_SEED = 0x7475666F
SIGNAL_STEP = 32
SAMPLE_TOLERANCE = 6
ANCHOR_STRIDE = 64
ANCHOR_LENGTH = 8


def signal_samples(seconds=SIGNAL_SECONDS):
    randomizer = random.Random(SIGNAL_SEED)
    return array.array("h", (randomizer.choice((-1, 1)) * randomizer.randrange(16, 193) * SIGNAL_STEP
                            for _ in range(RATE * seconds)))


def write_stereo(path, seconds, frequency=None, reference=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as output:
        output.setparams((2, 2, RATE, 0, "NONE", "not compressed"))
        for start in range(0, RATE * seconds, RATE):
            samples = array.array("h")
            for frame in range(start, min(start + RATE, RATE * seconds)):
                left = round(2400 * math.sin(math.tau * frequency * frame / RATE)) if frequency else 0
                right = reference[frame % len(reference)] if reference is not None else 0
                samples.extend((left, right))
            if sys.byteorder != "little":
                samples.byteswap()
            output.writeframes(samples.tobytes())


def channel_chunks(path, channel, skip_frames=0, chunk_frames=RATE):
    with wave.open(str(path), "rb") as audio:
        assert (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) == (2, 2, RATE), path
        audio.setpos(min(skip_frames, audio.getnframes()))
        while data := audio.readframes(chunk_frames):
            samples = array.array("h", data)
            if sys.byteorder != "little":
                samples.byteswap()
            yield samples[channel::2]


def silence_report(path, skip_seconds=2):
    longest = current = frames = 0
    peak = 0
    for samples in channel_chunks(path, 0, round(skip_seconds * RATE)):
        for sample in samples:
            frames += 1
            peak = max(peak, abs(sample))
            current = current + 1 if sample == 0 else 0
            longest = max(longest, current)
    return {"frames": frames, "peak": peak, "longest_silence_ms": longest * 1000 / RATE,
            "passed": frames >= RATE and peak > 100 and longest < RATE / 1000}


def signature(samples, offset):
    return tuple(round(samples[offset + frame] / SIGNAL_STEP) for frame in range(ANCHOR_LENGTH))


class Continuity:
    def __init__(self, reference):
        self.reference = reference
        self.anchors = {signature(reference, frame): frame
                        for frame in range(0, len(reference) - ANCHOR_LENGTH, ANCHOR_STRIDE)}
        self.position = None
        self.frames = 0
        self.mismatched_frames = 0
        self.skipped_frames = 0
        self.repeated_or_delayed_frames = 0
        self.events = []
        self.pending = array.array("h")
        self.unaligned_frames = 0

    def alignment(self, samples, start):
        for offset in range(start, len(samples) - ANCHOR_LENGTH + 1):
            position = self.anchors.get(signature(samples, offset))
            if position is not None and all(abs(samples[offset + frame] - self.reference[position + frame])
                                            <= SAMPLE_TOLERANCE for frame in range(ANCHOR_LENGTH)):
                return offset, position
        return None

    def feed(self, samples, final=False):
        self.pending.extend(samples)
        offset = 0
        stop = len(self.pending) if final else max(0, len(self.pending) - ANCHOR_LENGTH)
        while offset < stop:
            if self.position is not None and abs(self.pending[offset] - self.reference[self.position]) <= SAMPLE_TOLERANCE:
                self.position = (self.position + 1) % len(self.reference)
                offset += 1
                self.frames += 1
                continue
            alignment = self.alignment(self.pending, offset)
            if alignment is None:
                count = stop - offset
                self.mismatched_frames += count
                if self.position is not None:
                    self.unaligned_frames += count
                self.frames += count
                if self.position is not None:
                    self.position = (self.position + count) % len(self.reference)
                offset = stop
                break
            next_offset, position = alignment
            count = next_offset - offset
            if self.position is not None:
                self.mismatched_frames += count
                expected = (self.position + count) % len(self.reference)
                shift = (position - expected + len(self.reference) // 2) % len(self.reference) - len(self.reference) // 2
                self.skipped_frames += max(0, shift)
                self.repeated_or_delayed_frames += max(0, -shift)
                if len(self.events) < 32:
                    self.events.append({"capture_at_ms": self.frames * 1000 / RATE,
                                        "mismatched_frames": count + self.unaligned_frames,
                                        "source_shift_frames": shift})
                self.unaligned_frames = 0
            else:
                first = (position - count) % len(self.reference)
                self.mismatched_frames += sum(abs(self.pending[offset + frame] - self.reference[(first + frame) % len(self.reference)])
                                              > SAMPLE_TOLERANCE for frame in range(count))
            self.frames += count
            offset = next_offset
            self.position = position
            # Consume the anchor's first frame so a quantization error cannot retry it forever.
            self.position = (self.position + 1) % len(self.reference)
            self.frames += 1
            offset += 1
        del self.pending[:offset]

    def report(self):
        return {"frames": self.frames, "mismatched_frames": self.mismatched_frames,
                "skipped_frames": self.skipped_frames,
                "repeated_or_delayed_frames": self.repeated_or_delayed_frames,
                "events": self.events, "aligned": self.position is not None,
                "passed": self.position is not None and self.frames >= RATE
                and self.mismatched_frames == self.skipped_frames == self.repeated_or_delayed_frames == 0}


def continuity_report(path, reference, skip_seconds=2):
    continuity = Continuity(reference)
    for samples in channel_chunks(path, 1, round(skip_seconds * RATE)):
        continuity.feed(samples)
    continuity.feed(array.array("h"), final=True)
    return continuity.report()
