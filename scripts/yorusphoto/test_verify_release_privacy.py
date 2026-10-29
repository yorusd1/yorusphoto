"""Synthetic regressions for the public native-binary privacy gate."""

import io
import json
from pathlib import Path
import tempfile
import unittest

from verify_release_privacy import MAX_REPORTED_MATCHES, scan_stream, verify_native_binaries


class PrivacyGateTests(unittest.TestCase):
    def test_rejects_personal_roots_in_ascii_and_both_utf16_orders(self):
        paths = ("C:\\Users\\synthetic-person\\.cargo\\source.rs", "d:/users/synthetic-person/source.rs",
                 "/Users/synthetic-person/project/source.rs", "/home/synthetic-person/.cargo/source.rs")
        for encoding in ("ascii", "utf-16-le", "utf-16-be"):
            for path in paths:
                with self.subTest(encoding=encoding, root=path.split("synthetic")[0]):
                    report = scan_stream(io.BytesIO(b"binary\x00" + path.encode(encoding)))
                    self.assertFalse(report["passed"])
                    self.assertTrue(report["matchCounts"])
                    self.assertNotIn("synthetic-person", json.dumps(report))

    def test_accepts_remapped_compiler_paths(self):
        content = "\0/src/crates/ui/source.rs\0/cargo/registry/source.rs\0/rustc/revision/library/core.rs\0/build/toolchain/src.c"
        for encoding in ("ascii", "utf-16-le", "utf-16-be"):
            self.assertTrue(scan_stream(io.BytesIO(content.encode(encoding)))["passed"])

    def test_detects_roots_across_every_small_chunk_boundary_once(self):
        for encoding in ("ascii", "utf-16-le", "utf-16-be"):
            needle = "C:\\Users\\synthetic-person\\source.rs".encode(encoding)
            for padding in range(40):
                with self.subTest(encoding=encoding, padding=padding):
                    report = scan_stream(io.BytesIO(b"x" * padding + needle), chunk_bytes=17)
                    self.assertFalse(report["passed"])
                    self.assertEqual(sum(report["matchCounts"].values()), 1)

    def test_caps_matches_without_disclosing_identity(self):
        content = b"\0C:\\Users\\synthetic-person\\source.rs" * 100
        report = scan_stream(io.BytesIO(content), chunk_bytes=23)
        self.assertEqual(sum(report["matchCounts"].values()), 100)
        self.assertEqual(len(report["matches"]), MAX_REPORTED_MATCHES)
        self.assertNotIn("synthetic-person", json.dumps(report))

    def test_bounds_reads_and_rejects_oversize_stream(self):
        class BoundedStream(io.BytesIO):
            def read(self, count=-1):
                if not 0 < count <= 7:
                    raise AssertionError("Unbounded read")
                return super().read(count)
        self.assertFalse(scan_stream(BoundedStream(b"x" * 33), chunk_bytes=7, max_bytes=32)["passed"])
        self.assertTrue(scan_stream(BoundedStream(b"x" * 32), chunk_bytes=7, max_bytes=32)["passed"])

    def test_reports_basenames_and_safe_missing_file_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            safe = Path(directory) / "yorusphoto.exe"
            safe.write_bytes(b"\0/src/source.rs")
            report = verify_native_binaries([safe, Path(directory) / "absent.exe"])
            self.assertFalse(report["passed"])
            self.assertEqual(report["files"][0]["file"], "yorusphoto.exe")
            self.assertTrue(report["files"][0]["passed"])
            self.assertNotIn(directory, json.dumps(report))

    def test_rejects_empty_file_list(self):
        self.assertFalse(verify_native_binaries([])["passed"])


if __name__ == "__main__":
    unittest.main()
