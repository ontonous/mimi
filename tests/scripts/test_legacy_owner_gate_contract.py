from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
AUDIT = ROOT / "scripts/audit-mir-legacy-owners.sh"
PROBES = ROOT / "scripts/verify-mir-legacy-owner-probes.sh"
MAP_ANY_TEST = "compile_checked_fails_closed_on_reachable_map_any_unpack"


def mirror_source_with_overrides(
    source: Path,
    destination: Path,
    prefix: str,
    overrides: dict[str, str],
) -> None:
    destination.mkdir()
    for child in source.iterdir():
        relative = f"{prefix}/{child.name}"
        if relative in overrides:
            destination.joinpath(child.name).write_text(
                overrides[relative], encoding="utf-8"
            )
        elif any(path.startswith(relative + "/") for path in overrides):
            if not child.is_dir():
                raise AssertionError(f"override parent is not a directory: {relative}")
            mirror_source_with_overrides(
                child, destination / child.name, relative, overrides
            )
        else:
            (destination / child.name).symlink_to(
                child, target_is_directory=child.is_dir()
            )


def run_static_audit_with_overrides(overrides: dict[str, str]) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory(prefix="mimi-legacy-owner-static-") as temporary:
        temp_root = Path(temporary)
        for source_dir in ("tests", "std"):
            (temp_root / source_dir).symlink_to(
                ROOT / source_dir, target_is_directory=True
            )
        mirror_source_with_overrides(ROOT / "src", temp_root / "src", "src", overrides)
        script_dir = temp_root / "scripts"
        script_dir.mkdir()
        tampered_audit = script_dir / AUDIT.name
        tampered_audit.write_text(AUDIT.read_text(encoding="utf-8"), encoding="utf-8")
        tampered_audit.chmod(AUDIT.stat().st_mode)
        return subprocess.run(
            ["bash", str(tampered_audit), "--static-only"],
            cwd=temp_root,
            check=False,
            capture_output=True,
            text=True,
            env={**os.environ, "MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES": "0"},
        )


class LegacyOwnerGateContractTests(unittest.TestCase):
    def test_static_audit_keeps_all_four_original_owners_open_and_partial(self):
        result = subprocess.run(
            ["bash", str(AUDIT), "--static-only"],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            result.returncode,
            0,
            f"legacy-owner static audit failed:\n{result.stdout}\n{result.stderr}",
        )
        lines = result.stdout.splitlines()
        owners = [line for line in lines if line.startswith("legacy_gate_owner=")]
        expected = [
            "CodegenLegacyRemainder",
            "FlowVerifierCompatibility",
            "FfiVerifierCompatibility",
            "DualVerifierCompatibility",
        ]
        self.assertEqual(
            [line.split()[0].split("=", 1)[1] for line in owners], expected
        )
        for line in owners:
            self.assertIn(" production_call_edge=", line)
            self.assertIn(" legacy_consumer_edge=", line)
            self.assertIn(" capability_migration=open ", line)
            self.assertIn(" coverage_status=partial ", line)
            self.assertRegex(line, r" blocker=[^ ]+$")

        codegen_owner = next(line for line in owners if "CodegenLegacyRemainder" in line)
        self.assertIn("checked-native-MIR-or-Resolved-or-coverage;no-surface-AST-body", codegen_owner)
        self.assertIn("src/codegen/compile.rs::CodeGenerator.compile_checked->MIR-or-Resolved-checked-native-consumer[old-body-edge-absent]", codegen_owner)
        self.assertIn("native-AST-fifth-pass+test-adapters+orphan-body-physically-retired", codegen_owner)
        self.assertIn("original-Codegen-supported-capability-migration-and-behavior-regression-not-fully-proven", codegen_owner)
        self.assertNotIn("DefaultBytecodeAstCompatibility", codegen_owner)

        self.assertIn("legacy_gate_owner_count=4", lines)
        self.assertIn("owner_retirement_status=reachable-compatibility-surfaces-remain;route-and-boundary-evidence-only", lines)
        self.assertTrue(
            any(line.startswith("bytecode_vm_ffi_preflight_order=") for line in lines)
        )
        self.assertTrue(
            any(line.startswith("observer_v1_exact_route_binding=") for line in lines)
        )
        self.assertTrue(
            any(line.startswith("map_any_native_codegen_direct_probe_contract=") for line in lines)
        )
        self.assertTrue(
            any(line.startswith("native_codegen_ast_fifth_pass=physically-retired ") for line in lines)
        )
        self.assertIn(
            "native_codegen_ast_test_adapters=physically-absent old-body-call-refs=0",
            lines,
        )
        self.assertIn(
            "codegen_legacy_consumer_boundary=llvm_surface_file_adapter_absent;native_build_no_bytecode_legacy;run-test-disasm_explicit_legacy_route",
            lines,
        )
        self.assertTrue(
            any(line.startswith("default_bytecode_ast_compatibility=reachable status=open ") for line in lines)
        )
        self.assertTrue(
            any(line.startswith("owner=DefaultBytecodeAstCompatibility reachability=production ") for line in lines)
        )
        self.assertIn("static_audit_status=ok", lines)
        self.assertNotIn("route_boundary_audit_status=passed", lines)

    def test_direct_map_any_native_probe_is_scheduled_with_exact_execution_oracle(self):
        runner = PROBES.read_text(encoding="utf-8")
        source = (ROOT / "src/codegen/tests.rs").read_text(encoding="utf-8")
        test_start = source.index(f"fn {MAP_ANY_TEST}() {{")
        test_end = source.index("\n#[test]", test_start)
        body = source[test_start:test_end]

        self.assertIn(f"codegen::tests::{MAP_ANY_TEST}", runner)
        self.assertIn("strict_test_oracle", runner)
        self.assertIn('running 1 test', runner)
        self.assertIn('.compile_checked(&program)', body)
        self.assertIn("Some(crate::diagnostic::codes::E0722)", body)
        self.assertIn("compile_resolved_native(&program)", body)
        self.assertIn("compile_resolved_subset(&program", body)

    def test_map_any_native_tripwire_mutation_fails_static_audit(self):
        source = (ROOT / "src/codegen/tests.rs").read_text(encoding="utf-8")
        start = source.index(f"fn {MAP_ANY_TEST}() {{")
        end = source.index("\n#[test]", start)
        body = source[start:end]
        changed_body = body.replace(
            "Some(crate::diagnostic::codes::E0722)",
            "Some(crate::diagnostic::codes::E0721)",
            1,
        )
        self.assertNotEqual(body, changed_body)
        result = run_static_audit_with_overrides(
            {"src/codegen/tests.rs": source[:start] + changed_body + source[end:]}
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "owner_audit_error=codegen_closed_route_tripwire_missing_marker class=map-any-native-hard-gate test=compile_checked_fails_closed_on_reachable_map_any_unpack marker=Some(crate::diagnostic::codes::E0722)",
            result.stderr,
        )

    def test_bytecode_ffi_preflight_mutation_fails_static_audit(self):
        source = (ROOT / "src/interp/bytecode/vm.rs").read_text(encoding="utf-8")
        start = source.index("    fn preflight_canonical_program_structure(")
        end = source.index("\n    /// Validate direct call frames", start)
        body = source[start:end]
        changed_body = body.replace(
            "        self.validate_canonical_ffi_program()?;\n", "", 1
        )
        self.assertNotEqual(body, changed_body)
        result = run_static_audit_with_overrides(
            {"src/interp/bytecode/vm.rs": source[:start] + changed_body + source[end:]}
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "owner_audit_error=bytecode_vm_ffi_preflight_marker_or_order_missing",
            result.stderr,
        )

    def test_observer_profile_mutation_fails_static_audit(self):
        source = (ROOT / "src/main/canonical_dispatch.rs").read_text(encoding="utf-8")
        start = source.index("pub(crate) fn route_receipt_profile_for_checked(")
        end = source.index("\n}", start)
        body = source[start:end]
        changed_body = body.replace(
            "ORDINARY_MAP_OWNER_LIFECYCLE_OBSERVER_V1_PROFILE",
            "ORDINARY_MAP_OWNER_LIFECYCLE_I32_STRING_V1_ISLAND",
            1,
        )
        self.assertNotEqual(body, changed_body)
        result = run_static_audit_with_overrides(
            {
                "src/main/canonical_dispatch.rs": source[:start]
                + changed_body
                + source[end:]
            }
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "owner_audit_error=observer_v1_exact_route_binding_missing=",
            result.stderr,
        )


if __name__ == "__main__":
    unittest.main()
