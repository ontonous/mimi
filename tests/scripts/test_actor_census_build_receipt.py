from __future__ import annotations

import copy
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

import actor_census_build_receipt as receipt_tool
import full_route_census as census


def _write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True, ensure_ascii=False) + "\n", encoding="utf-8")


def _write_nul(path: Path, values: list[str]) -> None:
    path.write_bytes(b"\0".join(os.fsencode(value) for value in values) + b"\0")


def _refresh_evidence(receipt: dict, name: str, path: Path) -> None:
    data = path.read_bytes()
    receipt["evidence"][name]["bytes"] = len(data)
    receipt["evidence"][name]["sha256"] = hashlib.sha256(data).hexdigest()


def _record_capture(gate_dir: Path, prefix: str, command: list[str], *, cwd: Path) -> None:
    _write_nul(gate_dir / f"{prefix}.argv.nul", command)
    result = subprocess.run(command, cwd=cwd, capture_output=True, check=False)
    (gate_dir / f"{prefix}.exit.txt").write_text(
        f"exit_code={result.returncode}\n", encoding="utf-8"
    )
    (gate_dir / f"{prefix}.stdout.bin").write_bytes(result.stdout)
    (gate_dir / f"{prefix}.stderr.bin").write_bytes(result.stderr)


class ActorCensusBuildReceiptTests(unittest.TestCase):
    @contextmanager
    def _receipt_case(self) -> Iterator[dict[str, object]]:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            root = temporary_path / "repo"
            (root / "src").mkdir(parents=True)
            (root / "tests").mkdir()
            (root / "Cargo.toml").write_text("[package]\nname='receipt-fixture'\n", encoding="utf-8")
            (root / "Cargo.lock").write_text("# lock\n", encoding="utf-8")
            (root / ".cargo").mkdir()
            (root / ".cargo/config.toml").write_text(
                '[build]\nrustflags = ["-C", "debuginfo=0"]\n'
                '[registry]\ntoken = "fixture-secret-must-not-be-recorded"\n',
                encoding="utf-8",
            )
            (root / "src/lib.rs").write_text("pub fn f() {}\n", encoding="utf-8")
            source = root / "tests/fixture.mimi"
            source.write_text("func main() { println(1) }\n", encoding="utf-8")

            artifact_root = temporary_path / "evidence"
            gate_dir = artifact_root / "commands/current-source-mimi-build"
            gate_dir.mkdir(parents=True)
            target_dir = temporary_path / "fresh-cargo-target"
            target_dir.mkdir()
            host_target = "receipt-test-host"
            mimi_path = target_dir / host_target / "debug/mimi"
            mimi_path.parent.mkdir(parents=True)
            mimi_path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            mimi_path.chmod(0o755)

            receipt_path = gate_dir / "build-receipt.json"
            environment_path = gate_dir / "cargo-environment.json"
            environment = dict(os.environ)
            environment.pop("CARGO_BUILD_TARGET", None)
            environment.pop("CARGO_ENCODED_RUSTFLAGS", None)
            environment.update(
                {
                    "CARGO_TARGET_DIR": str(target_dir.resolve()),
                    "CARGO_BUILD_JOBS": "1",
                    "CARGO_TERM_COLOR": "never",
                }
            )
            environment_document = receipt_tool.environment_document(environment)
            environment_document["working_directory"] = str(root.resolve())
            _write_json(environment_path, environment_document)

            helper_path = str(SCRIPTS / "actor_census_build_receipt.py")
            capture_argv_path = gate_dir / "environment-capture.argv.nul"
            _write_nul(
                capture_argv_path,
                [sys.executable, helper_path, "capture-environment", "--output", str(environment_path)],
            )
            (gate_dir / "environment-capture.exit.txt").write_text("exit_code=0\n", encoding="utf-8")
            (gate_dir / "environment-capture.stdout.bin").write_bytes(b"")
            (gate_dir / "environment-capture.stderr.bin").write_bytes(b"")

            cargo_path = receipt_tool.resolve_selected_toolchain_cargo(root)
            cargo_argv = [
                str(cargo_path),
                "build",
                "--bin",
                "mimi",
                "--locked",
                "--target",
                host_target,
                "--message-format=json-render-diagnostics",
            ]
            _write_nul(gate_dir / "argv.nul", cargo_argv)
            (gate_dir / "exit.txt").write_text("exit_code=0\n", encoding="utf-8")
            cargo_event = {
                "reason": "compiler-artifact",
                "target": {"name": "mimi", "kind": ["bin"]},
                "executable": str(mimi_path.resolve()),
            }
            (gate_dir / "stdout.bin").write_bytes((json.dumps(cargo_event) + "\n").encode())
            (gate_dir / "stderr.bin").write_bytes(b"")
            identity_before = gate_dir / "cargo-identity.before.json"
            identity_after = gate_dir / "cargo-identity.after.json"
            config_before = gate_dir / "cargo-config.before.json"
            config_after = gate_dir / "cargo-config.after.json"
            for prefix, command in (
                (
                    "cargo-identity-before-capture",
                    [sys.executable, helper_path, "capture-cargo-identity", "--root", str(root),
                     "--cargo", str(cargo_path), "--output", str(identity_before)],
                ),
                (
                    "cargo-config-before-capture",
                    [sys.executable, helper_path, "capture-cargo-config", "--root", str(root),
                     "--output", str(config_before)],
                ),
                (
                    "cargo-identity-after-capture",
                    [sys.executable, helper_path, "capture-cargo-identity", "--root", str(root),
                     "--cargo", str(cargo_path), "--output", str(identity_after)],
                ),
                (
                    "cargo-config-after-capture",
                    [sys.executable, helper_path, "capture-cargo-config", "--root", str(root),
                     "--output", str(config_after)],
                ),
            ):
                _record_capture(gate_dir, prefix, command, cwd=root)
            (gate_dir / "fresh-target.txt").write_text(
                f"target_path={target_dir.resolve()}\ndirectory_existed_before=false\ncreated_exclusively=true\n",
                encoding="utf-8",
            )
            before = receipt_tool.capture_collector_snapshot(root, gate_dir / "source.before.json")
            after = receipt_tool.capture_collector_snapshot(root, gate_dir / "source.after.json")
            self.assertEqual(before, after)
            for label, snapshot_path in (
                ("source-before-capture", gate_dir / "source.before.json"),
                ("source-after-capture", gate_dir / "source.after.json"),
            ):
                _write_nul(
                    gate_dir / f"{label}.argv.nul",
                    [
                        sys.executable,
                        helper_path,
                        "capture-source-snapshot",
                        "--root",
                        str(root),
                        "--output",
                        str(snapshot_path),
                    ],
                )
                (gate_dir / f"{label}.exit.txt").write_text("exit_code=0\n", encoding="utf-8")
                (gate_dir / f"{label}.stdout.bin").write_bytes(b"")
                (gate_dir / f"{label}.stderr.bin").write_bytes(b"")

            built, issues = receipt_tool.create_receipt(
                artifact_root=artifact_root,
                gate_dir=gate_dir,
                target_dir=target_dir,
                host_target=host_target,
                root=root,
                receipt_path=receipt_path,
            )
            self.assertEqual(issues, [])
            _write_json(receipt_path, built) if not receipt_path.exists() else None
            yield {
                "root": root,
                "source": source,
                "artifact_root": artifact_root,
                "gate_dir": gate_dir,
                "target_dir": target_dir,
                "mimi_path": mimi_path,
                "receipt_path": receipt_path,
                "receipt": built,
                "snapshot": before,
            }

    def _validate(self, case: dict[str, object], *, snapshot: dict | None = None, receipt: dict | None = None) -> list[str]:
        return receipt_tool.validate_receipt(
            receipt if receipt is not None else case["receipt"],  # type: ignore[arg-type]
            receipt_path=case["receipt_path"],  # type: ignore[arg-type]
            root=case["root"],  # type: ignore[arg-type]
            mimi_path=case["mimi_path"],  # type: ignore[arg-type]
            report_snapshots={
                "source_snapshot": snapshot if snapshot is not None else case["snapshot"]
            },
        )

    def test_raw_receipt_recomputes_to_verified_identity(self) -> None:
        with self._receipt_case() as case:
            self.assertEqual(self._validate(case), [])
            receipt = case["receipt"]
            cargo_identity = receipt["cargo"]["executable"]  # type: ignore[index]
            self.assertEqual(cargo_identity["path"], receipt["cargo"]["argv"][0])  # type: ignore[index]
            self.assertRegex(cargo_identity["version"], r"^cargo\s+\d+\.\d+\.\d+")
            self.assertEqual(
                cargo_identity["sha256_before"], cargo_identity["sha256_after"]
            )
            config = receipt["build_source"]["cargo_config"]  # type: ignore[index]
            config_paths = {entry["path"]: entry for entry in config["before"]["files"]}
            root_config = str(case["root"] / ".cargo/config.toml")  # type: ignore[operator]
            self.assertTrue(config_paths[root_config]["present"])
            self.assertEqual(
                config_paths[root_config]["compiler_settings"]["build"]["rustflags"],
                ["-C", "debuginfo=0"],
            )
            receipt_json = json.dumps(receipt, ensure_ascii=False)
            self.assertNotIn("fixture-secret-must-not-be-recorded", receipt_json)
            self.assertEqual(
                receipt["build_source"]["source_attestation_sha256"],  # type: ignore[index]
                receipt_tool.source_attestation_digest(
                    receipt["build_source"]["manifest"]["manifest_sha256"],  # type: ignore[index]
                    receipt["build_source"]["cargo_config"],  # type: ignore[index]
                ),
            )
            report_attestation = census._build_receipt_source_attestation(receipt)
            self.assertEqual(
                report_attestation["combined_sha256"],  # type: ignore[index]
                receipt["build_source"]["source_attestation_sha256"],  # type: ignore[index]
            )
            self.assertEqual(
                report_attestation["cargo_config"],  # type: ignore[index]
                receipt["build_source"]["cargo_config"],  # type: ignore[index]
            )

    def test_synthetic_sys_executable_is_rejected_as_cargo(self) -> None:
        with self._receipt_case() as case:
            gate_dir = case["gate_dir"]  # type: ignore[assignment]
            argv_path = gate_dir / "argv.nul"
            argv = receipt_tool._read_argv(argv_path)
            argv[0] = sys.executable
            _write_nul(argv_path, argv)
            forged = copy.deepcopy(case["receipt"])
            forged["cargo"]["argv"] = argv  # type: ignore[index]
            _refresh_evidence(forged, "cargo_argv", argv_path)
            issues = self._validate(case, receipt=forged)  # type: ignore[arg-type]
            self.assertTrue(
                any(
                    "selected rustup Cargo" in issue or "Cargo identity path differs" in issue
                    for issue in issues
                ),
                issues,
            )

    def test_cargo_config_change_creation_and_deletion_fail_closed(self) -> None:
        with self._receipt_case() as case:
            config_dir = case["root"] / ".cargo"  # type: ignore[operator]
            config_file = config_dir / "config.toml"
            config_file.write_text(
                '[build]\nrustflags = ["-C", "opt-level=1"]\n', encoding="utf-8"
            )
            changed_issues = self._validate(case)
            self.assertTrue(
                any("Cargo config files changed" in issue for issue in changed_issues),
                changed_issues,
            )

            config_file.write_text('[build]\nrustflags = ["-C", "debuginfo=0"]\n', encoding="utf-8")
            (config_dir / "config").write_text("[build]\njobs = 2\n", encoding="utf-8")
            created_issues = self._validate(case)
            self.assertTrue(
                any("Cargo config files changed" in issue for issue in created_issues),
                created_issues,
            )

            (config_dir / "config").unlink()
            config_file.unlink()
            deleted_issues = self._validate(case)
            self.assertTrue(
                any("Cargo config files changed" in issue for issue in deleted_issues),
                deleted_issues,
            )

    def test_cargo_config_before_after_delta_fails_even_if_live_file_is_unchanged(self) -> None:
        with self._receipt_case() as case:
            gate_dir = case["gate_dir"]  # type: ignore[assignment]
            after_path = gate_dir / "cargo-config.after.json"
            after = json.loads(after_path.read_text(encoding="utf-8"))
            config_entry = next(
                entry for entry in after["files"]
                if entry["path"] == str(case["root"] / ".cargo/config.toml")  # type: ignore[operator]
            )
            config_entry["sha256"] = "0" * 64
            _write_json(after_path, after)
            forged = copy.deepcopy(case["receipt"])
            _refresh_evidence(forged, "cargo_config_after", after_path)
            issues = self._validate(case, receipt=forged)  # type: ignore[arg-type]
            self.assertTrue(
                any("Cargo config files changed during the build" in issue for issue in issues),
                issues,
            )

    def test_receipt_change_after_initial_parse_fails_at_finalization(self) -> None:
        with self._receipt_case() as case:
            receipt_path = case["receipt_path"]  # type: ignore[assignment]
            initial, initial_sha256 = receipt_tool.read_receipt(receipt_path)
            receipt_path.write_bytes(receipt_path.read_bytes() + b" ")
            issues = receipt_tool.validate_receipt(
                initial,
                receipt_path=receipt_path,
                root=case["root"],  # type: ignore[arg-type]
                mimi_path=case["mimi_path"],  # type: ignore[arg-type]
                expected_receipt_sha256=initial_sha256,
                verify_receipt_file=True,
                report_snapshots={"source_snapshot": case["snapshot"]},  # type: ignore[dict-item]
            )
            self.assertTrue(
                any("changed after its initial parse" in issue for issue in issues),
                issues,
            )

    def test_full_route_check_uses_one_parse_and_reopens_at_finalization(self) -> None:
        source = (SCRIPTS / "full_route_census.py").read_text(encoding="utf-8")
        self.assertIn("actor_census_build_receipt.read_receipt(", source)
        self.assertIn("expected_receipt_sha256=build_receipt_sha256", source)
        self.assertGreaterEqual(source.count("verify_receipt_file=False"), 2)
        self.assertIn("receipt_file_sha256(", source)
        self.assertIn("build receipt file changed between initial parse and report finalization", source)

    def test_final_symlink_swap_is_rejected_by_descriptor_open(self) -> None:
        with self._receipt_case() as case:
            target = case["gate_dir"] / "stdout.bin"  # type: ignore[operator]
            outside = case["artifact_root"] / "outside-capture.bin"  # type: ignore[operator]
            outside.write_bytes(b"attacker replacement\n")
            real_open = os.open
            injected = False

            def swap_before_open(path: object, flags: int, *args: object, **kwargs: object) -> int:
                nonlocal injected
                if Path(path) == target and not injected:
                    target.unlink()
                    target.symlink_to(outside)
                    injected = True
                return real_open(path, flags, *args, **kwargs)

            with mock.patch.object(receipt_tool.os, "open", side_effect=swap_before_open):
                issues = self._validate(case)
            self.assertTrue(injected)
            self.assertTrue(target.is_symlink())
            self.assertTrue(
                any("cargo_stdout" in issue and "symlinked" in issue for issue in issues),
                issues,
            )

    @unittest.skipUnless(hasattr(os, "O_NONBLOCK") and hasattr(os, "mkfifo"), "requires Linux FIFO flags")
    def test_fifo_evidence_is_rejected_without_blocking_and_regular_files_still_read(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            regular_file = root / "regular.bin"
            regular_file.write_bytes(b"regular evidence")
            self.assertEqual(
                receipt_tool._read_regular_file_nofollow(regular_file), b"regular evidence"
            )

            fifo_path = root / "evidence.fifo"
            os.mkfifo(fifo_path)
            real_open = os.open
            observed_nonblock = False

            def require_nonblocking_open(path: object, flags: int, *args: object, **kwargs: object) -> int:
                nonlocal observed_nonblock
                if Path(path) == fifo_path:
                    observed_nonblock = bool(flags & os.O_NONBLOCK)
                    self.assertTrue(observed_nonblock, "FIFO evidence must be opened with O_NONBLOCK")
                return real_open(path, flags, *args, **kwargs)

            with mock.patch.object(receipt_tool.os, "open", side_effect=require_nonblocking_open):
                with self.assertRaisesRegex(ValueError, "not a regular file"):
                    receipt_tool._read_regular_file_nofollow(fifo_path)
            self.assertTrue(observed_nonblock)

    def test_missing_or_self_asserted_receipt_fails_closed(self) -> None:
        with self._receipt_case() as case:
            receipt_path = case["receipt_path"]
            receipt_path.unlink()  # type: ignore[union-attr]
            missing_issues = self._validate(case, receipt={})
            self.assertTrue(any("receipt is absent" in issue for issue in missing_issues))
            forged = {
                "schema_version": receipt_tool.RECEIPT_SCHEMA,
                "status": "verified",
                "current_source_attested": True,
            }
            issues = self._validate(case, receipt=forged)
            self.assertTrue(any("raw evidence" in issue for issue in issues))
            self.assertTrue(any("Cargo argv" in issue for issue in issues))

    def test_forged_receipt_fields_do_not_override_raw_argv(self) -> None:
        with self._receipt_case() as case:
            forged = copy.deepcopy(case["receipt"])
            forged["cargo"]["argv"][-1] = "--fake"  # type: ignore[index]
            issues = self._validate(case, receipt=forged)  # type: ignore[arg-type]
            self.assertTrue(any("differs from the raw NUL argv" in issue for issue in issues))

    def test_old_or_path_hash_mismatched_report_snapshot_fails(self) -> None:
        with self._receipt_case() as case:
            changed = copy.deepcopy(case["snapshot"])
            changed["files"]["tests/fixture.mimi"]["sha256"] = "0" * 64  # type: ignore[index]
            changed["snapshot_id"] = receipt_tool._snapshot_id(changed["files"])  # type: ignore[arg-type]
            issues = self._validate(case, snapshot=changed)  # type: ignore[arg-type]
            self.assertTrue(any("differs path-by-path" in issue for issue in issues))

    def test_stale_live_source_snapshot_fails(self) -> None:
        with self._receipt_case() as case:
            source = case["source"]
            source.write_text("func main() { println(2) }\n", encoding="utf-8")  # type: ignore[union-attr]
            current = receipt_tool.capture_collector_snapshot(
                case["root"], case["artifact_root"] / "current.json"  # type: ignore[operator]
            )
            issues = self._validate(case, snapshot=current)
            self.assertTrue(any("differs path-by-path" in issue for issue in issues))

    def test_multiple_cargo_mimi_artifacts_are_rejected_even_after_rehash(self) -> None:
        with self._receipt_case() as case:
            output = case["gate_dir"] / "stdout.bin"  # type: ignore[operator]
            output.write_bytes(output.read_bytes() + output.read_bytes())
            forged = copy.deepcopy(case["receipt"])
            _refresh_evidence(forged, "cargo_stdout", output)
            forged["cargo"]["stdout_sha256"] = hashlib.sha256(output.read_bytes()).hexdigest()  # type: ignore[index]
            issues = self._validate(case, receipt=forged)  # type: ignore[arg-type]
            self.assertTrue(any("exactly one Cargo-reported Mimi" in issue for issue in issues))

    def test_out_of_target_cargo_executable_is_rejected_even_after_rehash(self) -> None:
        with self._receipt_case() as case:
            external = case["root"].parent / "escaped-mimi"  # type: ignore[union-attr]
            external.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            external.chmod(0o755)
            gate_dir = case["gate_dir"]  # type: ignore[assignment]
            output = gate_dir / "stdout.bin"  # type: ignore[operator]
            event = json.loads(output.read_text(encoding="utf-8"))
            event["executable"] = str(external.resolve())
            output.write_text(json.dumps(event) + "\n", encoding="utf-8")
            forged = copy.deepcopy(case["receipt"])
            _refresh_evidence(forged, "cargo_stdout", output)
            forged["cargo"]["stdout_sha256"] = hashlib.sha256(output.read_bytes()).hexdigest()  # type: ignore[index]
            issues = self._validate(case, receipt=forged)  # type: ignore[arg-type]
            self.assertTrue(any("escaped the fresh isolated target" in issue for issue in issues))

    def test_stale_or_modified_mimi_executable_is_rejected(self) -> None:
        with self._receipt_case() as case:
            mimi_path = case["mimi_path"]
            mimi_path.write_text("#!/bin/sh\nexit 9\n", encoding="utf-8")  # type: ignore[union-attr]
            issues = self._validate(case)
            self.assertTrue(any("executable changed after the build" in issue for issue in issues))

    def test_strict_check_without_receipt_cannot_self_attest(self) -> None:
        gate = {"passed": True, "checks": {}}
        census.apply_build_provenance_to_gate(
            gate,
            {"status": "caller_supplied_unverified", "current_source_attested": False},
            requested_check=True,
        )
        self.assertEqual(gate["build_provenance_status"], "caller_supplied_unverified")
        self.assertFalse(gate["current_source_attested"])
        self.assertFalse(gate["checks"]["source_bound_mimi_build_receipt"])
        self.assertFalse(gate["passed"])


if __name__ == "__main__":
    unittest.main()
