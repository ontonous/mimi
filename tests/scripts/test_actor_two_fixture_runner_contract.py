from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/verify-actor-two-fixture-current.sh"


class ActorTwoFixtureRunnerSourceContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = RUNNER.read_text(encoding="utf-8")

    def test_legacy_owner_audit_script_is_a_required_source_input(self) -> None:
        required_sources_start = self.source.index("local -a required_sources=(")
        required_sources_end = self.source.index("\n    )", required_sources_start)
        required_sources = self.source[required_sources_start:required_sources_end]
        self.assertIn("scripts/audit-mir-legacy-owners.sh", required_sources)

    def test_required_inputs_are_validated_and_hashed_by_before_after_snapshots(self) -> None:
        inventory_start = self.source.index("source_input_inventory() {")
        inventory_end = self.source.index("\n}\n\nsource_snapshot()", inventory_start)
        inventory = self.source[inventory_start:inventory_end]
        self.assertIn('if [[ ! -f "$required_path" || -L "$required_path" ]]', inventory)
        self.assertIn("printf '%s\\0' \"$required_path\"", inventory)

        snapshot_start = self.source.index("source_snapshot() {")
        snapshot_end = self.source.index("\n}\n\nvalidate_cargo_target_configuration()", snapshot_start)
        snapshot = self.source[snapshot_start:snapshot_end]
        self.assertIn('! sha256sum -- "$source_path" >> "$snapshot_tmp"', snapshot)
        self.assertIn('! cmp -s "$inventory_path" "$verify_inventory_path"', snapshot)


if __name__ == "__main__":
    unittest.main()
