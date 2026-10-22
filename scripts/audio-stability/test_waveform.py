"""Exercise detector failures using generated PCM, including errors a sine can hide."""

import array
import unittest

from waveform import Continuity, RATE, signal_samples


class ContinuityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.reference = signal_samples(2)

    def inspect(self, samples):
        detector = Continuity(self.reference)
        for start in range(0, len(samples), 13_117):
            detector.feed(samples[start:start + 13_117])
        detector.feed(array.array("h"), final=True)
        return detector.report()

    def test_loop_wraps_and_capture_offset_preserve_continuity(self):
        samples = (self.reference * 3)[137:-71]
        samples = array.array("h", (value + (index % 3) - 1 for index, value in enumerate(samples)))
        self.assertTrue(self.inspect(samples)["passed"])

    def test_missing_frames_are_detected_even_without_silence(self):
        samples = self.reference * 2
        del samples[RATE:RATE + 480]
        report = self.inspect(samples)
        self.assertFalse(report["passed"])
        self.assertEqual(report["skipped_frames"], 480)

    def test_repeated_frames_are_detected_even_without_silence(self):
        samples = self.reference[:RATE] + self.reference[RATE - 480:] + self.reference
        report = self.inspect(samples)
        self.assertFalse(report["passed"])
        self.assertEqual(report["repeated_or_delayed_frames"], 480)

    def test_silence_and_wrong_playback_rate_fail(self):
        samples = self.reference * 2
        samples[RATE:RATE + 480] = array.array("h", [0]) * 480
        self.assertFalse(self.inspect(samples)["passed"])
        self.assertFalse(self.inspect((self.reference * 2)[::2])["passed"])

    def test_initial_silence_cannot_be_hidden_by_finding_a_later_anchor(self):
        samples = array.array("h", [0]) * (RATE // 10) + self.reference
        report = self.inspect(samples)
        self.assertFalse(report["passed"])
        self.assertGreaterEqual(report["mismatched_frames"], RATE // 10)


if __name__ == "__main__":
    unittest.main()
