import json
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib
from pathlib import Path


class VisualAuditCliTest(unittest.TestCase):
    def test_pixel_failure_writes_report_and_returns_nonzero(self):
        script = Path(__file__).with_name("desktop-visual-audit.py")

        def png_chunk(kind: bytes, data: bytes) -> bytes:
            return (
                struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data))
            )

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            image = root / "seam.png"
            image.write_bytes(
                b"\x89PNG\r\n\x1a\n"
                + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 3, 1, 8, 0, 0, 0, 0))
                + png_chunk(b"IDAT", zlib.compress(bytes([0, 10, 20, 10])))
                + png_chunk(b"IEND", b"")
            )
            manifest = root / "manifest.json"
            report = root / "report.json"
            for threshold, passed, expected_exit in [(9, False, 1), (10, True, 0)]:
                with self.subTest(threshold=threshold):
                    manifest.write_text(json.dumps({"checks": [{
                        "name": "seam", "kind": "vertical_seam", "image": str(image),
                        "roi": [0, 0, 3, 1], "maxSpikeLuma": threshold,
                    }]}), encoding="utf-8")
                    result = subprocess.run(
                        [sys.executable, str(script), "--manifest", str(manifest),
                         "--output", str(report)],
                        check=False, capture_output=True, text=True,
                    )
                    payload = json.loads(report.read_text(encoding="utf-8"))
                    self.assertIs(payload["checks"]["seam"]["pass"], passed)
                    self.assertIs(payload["allPassed"], passed)
                    self.assertEqual(result.returncode, expected_exit, result.stderr)


if __name__ == "__main__":
    unittest.main()
