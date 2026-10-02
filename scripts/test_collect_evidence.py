#!/usr/bin/env python3
"""Isolated regression tests for evidence identity and source-bundle integrity.

All source inputs, archives and simulated Cargo output live in temporary toy
repositories. Toolchain probes are stubbed; Cargo and Rust are never invoked.
"""

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("collect_evidence", Path(__file__).with_name("collect-evidence.py"))
COLLECTOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COLLECTOR)


class EvidenceIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="mimi-evidence-integrity-test-")
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name)
        self.root = self.parent / "toy-repository"
        self.root.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "-c", "user.name=Evidence fixture", "-c",
                        "user.email=evidence-fixture@example.invalid", "commit", "--allow-empty", "-qm",
                        "isolated synthetic fixture"], cwd=self.root, check=True)
        (self.root / "src").mkdir()
        (self.root / "tests").mkdir()
        (self.root / "src/input.rs").write_bytes(b"fn toy_input() {}\n")
        (self.root / "tests/input.mimi").write_bytes(b"func main() -> i32 {43}\n")
        self.probe_stub = patch.object(COLLECTOR, "toolchain", return_value={
            "status": "synthetic fixture: no Cargo, Rust or LLVM probes"})
        self.probe_stub.start()
        self.addCleanup(self.probe_stub.stop)

    def arguments(self, name):
        evidence = self.parent / name
        evidence.mkdir()
        return SimpleNamespace(root=self.root, input=[], evidence=evidence, artifact_file=[], command=[])

    def python_command(self, code):
        return [sys.executable, "-c", code]

    def test_bundle_reproduces_every_input_byte_and_external_symlink_metadata(self):
        external = self.parent / "external-source"
        external.write_bytes(b"external fixture bytes\n")
        (self.root / "src/external.rs").symlink_to(external)
        before = COLLECTOR.snapshot(self.root, [])
        args = self.arguments("complete")
        bundle = COLLECTOR.archive_sources(args, before)
        self.assertEqual(bundle["source_identity"], before["source_identity"])
        self.assertEqual(bundle["archive"]["sha256"], COLLECTOR.sha(args.evidence / "source-bundle.tar.gz"))
        self.assertEqual(bundle["manifest"]["sha256"], COLLECTOR.sha(args.evidence / "source-bundle.json"))
        with tarfile.open(args.evidence / "source-bundle.tar.gz", "r:gz") as archive:
            inner = json.loads(archive.extractfile("bundle-manifest.json").read())
            self.assertEqual(inner["source_identity"], before["source_identity"])
            self.assertEqual(inner["sources"], before["sources"])
            for member in bundle["members"]:
                content = archive.extractfile(member["member"]).read()
                self.assertEqual(hashlib.sha256(content).hexdigest(), member["sha256"])
                self.assertEqual(len(content), member["bytes"])
            for name, source in before["sources"].items():
                prefix = "symlink-target-contents/" if "symlink" in source else "sources/"
                content = archive.extractfile(prefix + name).read()
                self.assertEqual(hashlib.sha256(content).hexdigest(), source["sha256"])
            link = archive.getmember("sources/src/external.rs")
            self.assertTrue(link.issym())
            self.assertEqual(link.linkname, str(external))
            self.assertTrue(bundle["symlinks"][0]["external_target"])

    def test_wrong_source_bytes_are_rejected_and_failure_is_retained(self):
        before = COLLECTOR.snapshot(self.root, [])
        args = self.arguments("wrong-bytes")
        (self.root / "src/input.rs").write_bytes(b"changed after snapshot\n")
        with self.assertRaisesRegex(RuntimeError, "content changed"):
            COLLECTOR.archive_sources(args, before)
        failure = json.loads((args.evidence / "source-bundle-failure.json").read_text())
        self.assertFalse(failure["accepted"])
        self.assertEqual(failure["source_identity"], before["source_identity"])
        self.assertFalse((args.evidence / "source-bundle.json").exists())

    def test_existing_bundle_and_manifest_cannot_be_overwritten(self):
        before = COLLECTOR.snapshot(self.root, [])
        args = self.arguments("immutable")
        COLLECTOR.archive_sources(args, before)
        names = ("source-bundle.tar.gz", "source-bundle.json")
        hashes = {name: COLLECTOR.sha(args.evidence / name) for name in names}
        with self.assertRaisesRegex(RuntimeError, "preserve existing"):
            COLLECTOR.archive_sources(args, before)
        self.assertEqual(hashes, {name: COLLECTOR.sha(args.evidence / name) for name in names})

    def test_storage_budget_rejects_before_copying_inputs(self):
        before = COLLECTOR.snapshot(self.root, [])
        args = self.arguments("storage-budget")
        with patch.object(COLLECTOR, "MAX_SOURCE_INPUT_BYTES", 1):
            with self.assertRaisesRegex(RuntimeError, "exceed.*budget"):
                COLLECTOR.archive_sources(args, before)
        self.assertFalse((args.evidence / "source-bundle.tar.gz").exists())
        self.assertFalse(json.loads((args.evidence / "source-bundle-failure.json").read_text())["accepted"])

    def test_change_after_archiving_prevents_command_execution(self):
        before = COLLECTOR.snapshot(self.root, [])
        args = self.arguments("after-archive-change")
        COLLECTOR.archive_sources(args, before)
        (self.root / "src/input.rs").write_bytes(b"new source revision\n")
        marker = self.parent / "command-ran"
        command = self.python_command(f"from pathlib import Path; Path({str(marker)!r}).write_text('ran')")
        with self.assertRaisesRegex(RuntimeError, "source differs"):
            COLLECTOR.execute(args, "must-not-run", command, before)
        self.assertFalse(marker.exists())
        self.assertFalse((args.evidence / "must-not-run.log").exists())

    def test_zero_executable_build_keeps_exit_zero_but_is_not_accepted(self):
        args = self.arguments("zero-artifacts")
        args.command = self.python_command("print('{\"reason\":\"build-finished\",\"success\":true}')")
        self.assertEqual(COLLECTOR.build(args), 2)
        record = json.loads((args.evidence / "build.json").read_text())
        self.assertEqual(record["exit_code"], 0)
        self.assertFalse(record["accepted"])
        self.assertEqual(record["executable_artifact_count"], 0)
        self.assertTrue(record["source_bundle"]["accepted"])

    def test_artifact_without_cargo_completion_is_not_an_accepted_build(self):
        args = self.arguments("missing-cargo-completion")
        fake = self.parent / "synthetic-executable"
        fake.write_bytes(b"not a language executable; only a collector fixture")
        entry = {"reason": "compiler-artifact", "package_id": "synthetic-fixture", "target": {
            "name": "toy", "kind": ["test"]}, "profile": {"test": True},
            "filenames": [str(fake)], "executable": str(fake), "fresh": False}
        args.command = self.python_command(f"print({json.dumps(entry)!r})")
        self.assertEqual(COLLECTOR.build(args), 2)
        record = json.loads((args.evidence / "build.json").read_text())
        self.assertEqual(record["exit_code"], 0)
        self.assertEqual(record["executable_artifact_count"], 1)
        self.assertFalse(record["cargo_build_finished_success"])
        self.assertFalse(record["accepted"])

    def test_zero_tests_failure_exit_and_replaced_artifact_remain_rejected(self):
        args = self.arguments("execution-integrity")
        before = COLLECTOR.snapshot(self.root, [])
        code = "print('running 0 tests'); print('test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out')"
        zero = COLLECTOR.execute(args, "zero-tests", self.python_command(code), before, required_tests=True)
        self.assertEqual(zero["exit_code"], 0)
        self.assertFalse(zero["accepted"])
        failed = COLLECTOR.execute(args, "exit-seven", self.python_command("raise SystemExit(7)"), before)
        self.assertEqual(failed["exit_code"], 7)
        self.assertFalse(failed["accepted"])
        fake = self.parent / "replaceable-artifact"
        fake.write_bytes(b"first version")
        stored = COLLECTOR.artifact(fake)
        fake.write_bytes(b"second version")
        with self.assertRaisesRegex(RuntimeError, "artifact missing or changed"):
            COLLECTOR.verify_artifacts([stored])


if __name__ == "__main__":
    unittest.main(verbosity=2)
