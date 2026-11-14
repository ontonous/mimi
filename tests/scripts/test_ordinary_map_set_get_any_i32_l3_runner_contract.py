from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/verify-ordinary-map-set-get-any-i32-l3.sh"


class OrdinaryMapSetGetAnyI32L3RunnerContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = RUNNER.read_text(encoding="utf-8")

    def test_target_directory_must_be_absolute_and_canonically_external(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mimi-map-getany-l3-contract-") as temporary:
            temporary_root = Path(temporary)
            external = temporary_root / "external"
            external.mkdir()
            evidence = external / "fresh-evidence"
            target_alias = external / "target-alias"
            target_alias.symlink_to(ROOT, target_is_directory=True)
            harness = self._sandbox(temporary_root)

            missing = self._run(harness, target=None, evidence=evidence)
            self.assertEqual(missing.returncode, 2, missing.stderr)
            self.assertIn("CARGO_TARGET_DIR must name", missing.stderr)

            relative = self._run(harness, target="relative/target", evidence=evidence)
            self.assertEqual(relative.returncode, 2, relative.stderr)
            self.assertIn("CARGO_TARGET_DIR must be absolute", relative.stderr)

            internal = self._run(
                harness, target=ROOT / "target" / "map-getany-contract", evidence=evidence
            )
            self.assertEqual(internal.returncode, 2, internal.stderr)
            self.assertIn(
                "CARGO_TARGET_DIR must resolve outside the repository", internal.stderr
            )

            symlinked_internal = self._run(harness, target=target_alias, evidence=evidence)
            self.assertEqual(symlinked_internal.returncode, 2, symlinked_internal.stderr)
            self.assertIn(
                "CARGO_TARGET_DIR must resolve outside the repository",
                symlinked_internal.stderr,
            )
            self.assertFalse(evidence.exists(), "rejected target paths must not create evidence")
            self.assertFalse(
                harness[1].exists(), "rejected target paths must not invoke tools"
            )

    def test_evidence_directory_must_be_external_and_fresh(self) -> None:
        with tempfile.TemporaryDirectory(prefix="mimi-map-getany-l3-contract-") as temporary:
            temporary_root = Path(temporary)
            external = temporary_root / "external"
            external.mkdir()
            target = external / "target"
            evidence_inside_repo = ROOT / "target" / "map-getany-contract-evidence"
            existing_evidence = external / "already-exists"
            existing_evidence.mkdir()
            harness = self._sandbox(temporary_root)

            internal = self._run(
                harness, target=target, evidence=evidence_inside_repo
            )
            self.assertEqual(internal.returncode, 2, internal.stderr)
            self.assertIn("L3 evidence must be outside the repository", internal.stderr)

            stale = self._run(harness, target=target, evidence=existing_evidence)
            self.assertEqual(stale.returncode, 2, stale.stderr)
            self.assertIn("refusing to overwrite L3 evidence", stale.stderr)
            self.assertFalse(
                harness[1].exists(), "rejected evidence paths must not invoke tools"
            )

    def test_normalized_target_is_recorded_in_both_evidence_commands(self) -> None:
        self.assertIn('target_dir="$(realpath -m -- "$target_dir")"', self.source)
        self.assertIn('export CARGO_TARGET_DIR="$target_dir"', self.source)
        self.assertIn(
            "LLVM_SYS_181_PREFIX=%q CARGO_TARGET_DIR=%q MIMI_ORDINARY_MAP_SET_GET_ANY_I32_L3_EVIDENCE=%q",
            self.source,
        )
        self.assertIn('printf \'CARGO_TARGET_DIR=%s\\n\' "$target_dir"', self.source)

    def _sandbox(self, root: Path) -> tuple[dict[str, str], Path]:
        binaries = root / "bin"
        binaries.mkdir(exist_ok=True)
        tool_log = root / "tool-invocations.log"
        for command in ("cargo", "rustc", "valgrind"):
            path = binaries / command
            path.write_text(
                "#!/bin/sh\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" "
                ">> \"$L3_CONTRACT_TOOL_LOG\"\nexit 97\n",
                encoding="utf-8",
            )
            path.chmod(0o755)
        llvm_prefix = root / "llvm-wrapper"
        llvm_prefix.mkdir(exist_ok=True)
        environment = os.environ.copy()
        environment["PATH"] = f"{binaries}{os.pathsep}{environment.get('PATH', '')}"
        environment["L3_CONTRACT_TOOL_LOG"] = str(tool_log)
        environment["LLVM_SYS_181_PREFIX"] = str(llvm_prefix)
        return environment, tool_log

    def _run(
        self,
        harness: tuple[dict[str, str], Path],
        *,
        target: Path | str | None,
        evidence: Path,
    ) -> subprocess.CompletedProcess[str]:
        environment = dict(harness[0])
        if target is None:
            environment.pop("CARGO_TARGET_DIR", None)
        else:
            environment["CARGO_TARGET_DIR"] = str(target)
        environment["MIMI_ORDINARY_MAP_SET_GET_ANY_I32_L3_EVIDENCE"] = str(evidence)
        return subprocess.run(
            ["bash", str(RUNNER)],
            cwd=ROOT,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )


if __name__ == "__main__":
    unittest.main()
