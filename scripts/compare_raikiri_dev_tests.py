import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).with_name("compare-raikiri-dev.py")
spec = importlib.util.spec_from_file_location("compare_raikiri_dev", SCRIPT)
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)


class ComparisonTests(unittest.TestCase):
    def test_ppm_preserves_binary_whitespace_and_comments(self):
        data = b"P6\n# Poppler output\n2 1\n255\n" + bytes([10, 32, 35, 255, 0, 1])
        self.assertEqual(comparison.read_ppm(data), (2, 1, bytes([10, 32, 35, 255, 0, 1])))

    def test_ppm_rejects_truncated_payload(self):
        with self.assertRaises(ValueError):
            comparison.read_ppm(b"P6\n2 1\n255\n\x00\x00\x00")

    def test_rgb_difference_counts_pixels_and_channels(self):
        first = (2, 1, bytes([0, 0, 0, 255, 0, 0]))
        second = (2, 1, bytes([1, 4, 0, 255, 0, 0]))
        self.assertEqual(comparison.difference(first, second), {
            "same_dimensions": True, "different_pixels": 1, "maximum_channel_difference": 4,
        })

    def test_time_measurements_parse_labeled_colons(self):
        stderr = "Elapsed (wall clock) time (h:mm:ss or m:ss): 1:02.25\nMaximum resident set size (kbytes): 12345\n"
        self.assertEqual(comparison.measurements(stderr), {
            "elapsed_seconds": 62.25, "maximum_rss_kib": 12345,
        })

    def test_dimension_difference_is_reported(self):
        self.assertEqual(comparison.difference((1, 1, b"\0" * 3), (2, 1, b"\0" * 6)), {
            "same_dimensions": False, "different_pixels": None, "maximum_channel_difference": None,
        })


if __name__ == "__main__":
    unittest.main()
