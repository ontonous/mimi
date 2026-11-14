from __future__ import annotations

import importlib.util
from pathlib import Path
import re
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_DIR = ROOT / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))
GENERATOR_PATH = SCRIPTS_DIR / "gen-stdlib-matrix.py"

EXPECTED_MAP_APIS = {
    "new",
    "get",
    "set",
    "has_key",
    "remove",
    "size",
    "from_list",
    "get_or_default",
    "merge",
    "to_list",
    "filter_keys",
    "map_values",
    "update",
    "pick",
    "omit",
}
EXPECTED_RESULT_RECORD_APIS = {
    "new",
    "set",
    "remove",
    "merge",
    "filter_keys",
    "map_values",
    "update",
    "pick",
    "omit",
}


def load_generator():
    spec = importlib.util.spec_from_file_location(
        "stdlib_matrix_generator_maps_test", GENERATOR_PATH
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {GENERATOR_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


generator = load_generator()


class StdlibMatrixMapsGenerationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        # load_modules() reads the current std/*.mimi files; gen_single() builds
        # the source in memory and does not read the ignored generated directory.
        cls.maps_functions = generator.load_modules()["maps"]
        generator._LET_SEQ = 0
        cls.probe = generator.gen_single("maps", cls.maps_functions)
        cls.functions_by_name = {fn["name"]: fn for fn in cls.maps_functions}

    def section(self, api_name: str) -> str:
        marker = f"    // maps.{api_name}"
        lines = self.probe.splitlines()
        start = lines.index(marker) + 1
        end = next(
            (
                index
                for index in range(start, len(lines))
                if lines[index].startswith("    // maps.")
            ),
            len(lines),
        )
        return "\n".join(lines[start:end])

    def test_all_fifteen_current_public_apis_are_covered(self) -> None:
        names = {fn["name"] for fn in self.maps_functions}
        self.assertEqual(len(names), 15)
        self.assertEqual(names, EXPECTED_MAP_APIS)

        covered = {
            fn["name"]
            for fn in self.maps_functions
            if (
                (override := generator.OVERRIDES.get(f"maps.{fn['name']}")) is not None
                and not override.get("skip")
            )
            or generator.auto_args(fn["params"]) is not None
        }
        self.assertEqual(covered, names)
        self.assertNotRegex(self.probe, r"(?m)^\s*// (?:SKIP|TODO-UNCOVERED)\b")
        for name in names:
            with self.subTest(api=name):
                self.assertEqual(
                    sum(
                        line == f"    // maps.{name}"
                        for line in self.probe.splitlines()
                    ),
                    1,
                )

    def test_result_record_apis_size_and_drop_only_the_ok_payload(self) -> None:
        result_record_names = {
            fn["name"]
            for fn in self.maps_functions
            if fn["ret"] == "Result<Record, i32>"
        }
        self.assertEqual(result_record_names, EXPECTED_RESULT_RECORD_APIS)

        for name in result_record_names:
            with self.subTest(api=name):
                section = self.section(name)
                self.assertRegex(section, rf"\bmatch\s+{re.escape(name)}\(")
                self.assertIn("Ok(v) => { println(size(v)); drop(v) }", section)
                self.assertIn("Err(e) => { println(e) }", section)
                self.assertNotIn("println(v)", section)
                self.assertNotRegex(
                    section,
                    rf"\bsize\s*\(\s*{re.escape(name)}\s*\(",
                )
                result_binding = re.search(
                    rf"\blet\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*{re.escape(name)}\s*\(",
                    section,
                )
                if result_binding is not None:
                    self.assertNotRegex(
                        section,
                        rf"\bsize\s*\(\s*{re.escape(result_binding.group(1))}\b",
                    )

    def test_record_inputs_and_outputs_are_released(self) -> None:
        direct_record_names = {
            fn["name"] for fn in self.maps_functions if fn["ret"] == "Record"
        }
        self.assertEqual(direct_record_names, {"from_list"})

        record_input_names = {
            fn["name"]
            for fn in self.maps_functions
            if any(ty == "Record" for _name, ty in fn["params"])
        }
        self.assertEqual(
            record_input_names,
            EXPECTED_MAP_APIS - {"new", "from_list"},
        )

        for name in record_input_names:
            with self.subTest(api=name):
                section = self.section(name)
                input_names = re.findall(
                    r"(?m)^\s*let\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*map_from_list\(",
                    section,
                )
                self.assertEqual(len(input_names), sum(
                    ty == "Record"
                    for _param, ty in self.functions_by_name[name]["params"]
                ))
                for input_name in input_names:
                    calls = list(
                        re.finditer(
                            rf"\b{re.escape(name)}\s*\(([^)]*)\)", section
                        )
                    )
                    self.assertTrue(
                        any(
                            re.search(rf"\b{re.escape(input_name)}\b", call.group(1))
                            for call in calls
                        ),
                        f"{name} must receive {input_name}",
                    )
                    self.assertIn(f"drop({input_name});", section)
                    self.assertLess(
                        next(
                            call.start()
                            for call in calls
                            if re.search(
                                rf"\b{re.escape(input_name)}\b", call.group(1)
                            )
                        ),
                        section.index(f"drop({input_name});"),
                    )

        from_list = self.section("from_list")
        self.assertIn("let mfl_src = map_from_list(", from_list)
        self.assertIn("let mfl_rt = from_list(to_list(mfl_src));", from_list)
        self.assertIn("println(size(mfl_rt));", from_list)
        self.assertIn("drop(mfl_rt);", from_list)
        self.assertIn("drop(mfl_src);", from_list)
        self.assertLess(
            from_list.index("println(size(mfl_rt));"),
            from_list.index("drop(mfl_rt);"),
        )

        to_list = self.section("to_list")
        self.assertIn("let tl_input = map_from_list([]);", to_list)
        list_result = re.search(
            r"(?m)^\s*let\s+(tl_[A-Za-z0-9_]+)\s*=\s*to_list\(tl_input\);",
            to_list,
        )
        self.assertIsNotNone(list_result)
        list_name = list_result.group(1)
        self.assertIn(f"println(len({list_name}));", to_list)
        self.assertIn(f"drop({list_name});", to_list)
        self.assertIn("drop(tl_input);", to_list)
        self.assertLess(
            to_list.index(f"println(len({list_name}));"),
            to_list.index(f"drop({list_name});"),
        )


if __name__ == "__main__":
    unittest.main()
