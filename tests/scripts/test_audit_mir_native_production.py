from __future__ import annotations

import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
AUDIT_SCRIPT = ROOT / "scripts" / "audit-mir-native-production.sh"


class NativeProductionAuditTests(unittest.TestCase):
    def test_embedded_scanner_regressions(self) -> None:
        completed = subprocess.run(
            ["bash", str(AUDIT_SCRIPT), "--self-test"],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        self.assertIn("native_production_audit_self_test=ok", completed.stdout)


if __name__ == "__main__":
    unittest.main()
