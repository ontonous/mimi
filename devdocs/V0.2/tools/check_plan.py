#!/usr/bin/env python3
"""Check the local 0.2 planning structure; never certify compiler or release results."""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import sys
import tomllib
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
TASK_STATES = {"未开始", "设计中", "实现中", "验证中", "已验收", "阻塞", "研究中"}
DECISION_STATES = {"待裁决", "设计已接受", "实现已验证", "规范已生效", "重新审议"}
ACCEPTED_DESIGNS = {"设计已接受", "实现已验证", "规范已生效"}
PHASE = dict(B="P0", V="P1", M="P2", F="P3", E="P4", C="P4", K="P4", Q="P5", A="P6")
GOALS = {f"G{i}" for i in range(1, 8)}


def expand_ids(value: str) -> list[str]:
    def expand(match: re.Match[str]) -> str:
        left, start, right, stop = match.groups()
        if left != right or int(start) > int(stop):
            raise ValueError(f"invalid dependency range: {match.group(0)}")
        return "、".join(f"{left}-{i:02d}" for i in range(int(start), int(stop) + 1))

    value = re.sub(r"([A-Z])-(\d{2})[～~]([A-Z])-(\d{2})", expand, value.strip())
    if value == "无":
        return []
    items = re.split(r"[、,，]\s*", value)
    if any(not re.fullmatch(r"[A-Z]-\d{2}", item) for item in items):
        raise ValueError(f"dependencies must be explicit task/decision IDs: {value}")
    if len(items) != len(set(items)):
        raise ValueError(f"duplicate dependencies: {value}")
    return items


def definitions(root: Path) -> tuple[dict, set[str]]:
    tasks = {}
    for line in (root / "02-阶段与任务.md").read_text().splitlines():
        if not re.match(r"^\| [BVMFECKQA]-\d{2} \|", line):
            continue
        cells = [cell.strip() for cell in line.split("|")[1:-1]]
        if len(cells) != 4:
            raise ValueError(f"invalid task table row: {line}")
        task_id, level, description, _ = cells
        if task_id in tasks:
            raise ValueError(f"duplicate task definition: {task_id}")
        dependency = re.search(r"；依赖(.+)$", description)
        if not dependency:
            raise ValueError(f"missing explicit dependency clause: {task_id}")
        ids = expand_ids(dependency.group(1))
        if not level.startswith(("必需／", "实验／")):
            raise ValueError(f"invalid task level: {task_id}")
        tasks[task_id] = {
            "phase": PHASE[task_id[0]],
            "required": level.startswith("必需／"),
            "depends_on": sorted(item for item in ids if not item.startswith("D-")),
            "decision_dependencies": sorted(item for item in ids if item.startswith("D-")),
        }
    rows = re.findall(r"^\| (D-\d{2}) \|", (root / "05-待裁决问题.md").read_text(), re.M)
    if len(rows) != len(set(rows)):
        raise ValueError("duplicate decision definitions")
    goal_map = {key: set() for key in tasks}
    for line in (root / "README.md").read_text().splitlines():
        if not re.match(r"^\| G[1-7] \|", line):
            continue
        cells = [cell.strip() for cell in line.split("|")[1:-1]]
        text = re.sub(r"[A-Z]-\d{2}[～~][A-Z]-\d{2}",
                      lambda match: "、".join(expand_ids(match.group(0))), cells[1])
        for task_id in re.findall(r"[BVMFECKQA]-\d{2}", text):
            if task_id not in tasks:
                raise ValueError(f"unknown task in goal mapping: {task_id}")
            goal_map[task_id].add(cells[0])
    for task_id, task in tasks.items():
        if task_id.startswith("B-"):
            goal_map[task_id].update(GOALS)
        if not goal_map[task_id]:
            raise ValueError(f"no documented goal for {task_id}")
        task["goals"] = sorted(goal_map[task_id])
    return tasks, set(rows)


def validate_manifest(data: dict, expected: dict, decision_ids: set[str]) -> list[str]:
    errors = []
    if data.get("schema_version") != 1:
        errors.append("schema_version must be 1")
    if data.get("task_definitions") != "02-阶段与任务.md" or data.get("decision_definitions") != "05-待裁决问题.md":
        errors.append("definition source mismatch")
    tasks_list = data.get("tasks", [])
    decisions_list = data.get("decisions", [])
    if not isinstance(tasks_list, list) or not isinstance(decisions_list, list):
        return errors + ["tasks/decisions must be arrays"]
    if any(not isinstance(row, dict) or not isinstance(row.get("id"), str)
           for row in tasks_list + decisions_list):
        return errors + ["every task/decision needs a string id"]
    for label, rows in (("task", tasks_list), ("decision", decisions_list)):
        duplicates = [key for key, count in Counter(row["id"] for row in rows).items() if count > 1]
        errors.extend(f"duplicate {label}: {key}" for key in duplicates)
    tasks = {row["id"]: row for row in tasks_list}
    decisions = {row["id"]: row for row in decisions_list}
    if set(tasks) != set(expected):
        errors.append(f"task inventory mismatch: {sorted(set(tasks) ^ set(expected))}")
    if set(decisions) != decision_ids:
        errors.append(f"decision inventory mismatch: {sorted(set(decisions) ^ decision_ids)}")

    for decision_id, row in decisions.items():
        if row.get("status") not in DECISION_STATES:
            errors.append(f"{decision_id}: invalid decision status")
        refs = row.get("evidence")
        if not isinstance(refs, list) or any(not isinstance(ref, str) or not ref.strip() for ref in refs):
            errors.append(f"{decision_id}: invalid evidence references")
        elif row.get("status") != "待裁决" and not refs:
            errors.append(f"{decision_id}: changed decision status needs evidence")

    for task_id, row in tasks.items():
        for field in ("depends_on", "decision_dependencies", "goals", "evidence"):
            value = row.get(field)
            if not isinstance(value, list) or any(not isinstance(item, str) or not item.strip() for item in value):
                errors.append(f"{task_id}: invalid {field}")
                return errors
            if len(value) != len(set(value)):
                errors.append(f"{task_id}: duplicate {field}")
        status = row.get("status")
        if status not in TASK_STATES:
            errors.append(f"{task_id}: invalid task status")
        if not isinstance(row.get("blocked_reason"), str):
            errors.append(f"{task_id}: blocked_reason must be text")
            return errors
        if type(row.get("required")) is not bool:
            errors.append(f"{task_id}: required must be boolean")
        if not row["goals"] or not set(row["goals"]) <= GOALS:
            errors.append(f"{task_id}: missing or unknown goal")
        if task_id in expected:
            for field, wanted in expected[task_id].items():
                actual = sorted(row[field]) if isinstance(wanted, list) else row.get(field)
                if actual != wanted:
                    errors.append(f"{task_id}: {field} differs from task document")
        if status == "研究中" and row.get("required"):
            errors.append(f"{task_id}: required task cannot be research-only")
        if status == "阻塞" and not row.get("blocked_reason", "").strip():
            errors.append(f"{task_id}: blocked task needs a concrete reason")
        if status == "已验收" and not row["evidence"]:
            errors.append(f"{task_id}: accepted task needs evidence")
        for dependency in row["depends_on"]:
            if dependency not in tasks:
                errors.append(f"{task_id}: unknown prerequisite {dependency}")
            elif status == "已验收" and tasks[dependency].get("status") != "已验收":
                errors.append(f"{task_id}: prerequisite {dependency} is not accepted")
        for decision_id in row["decision_dependencies"]:
            if decision_id not in decisions:
                errors.append(f"{task_id}: unknown decision {decision_id}")
            elif status == "已验收" and decisions[decision_id].get("status") not in ACCEPTED_DESIGNS:
                errors.append(f"{task_id}: decision {decision_id} has not been accepted")

    active, done = [], set()

    def visit(task_id: str) -> None:
        if task_id in active:
            errors.append("dependency cycle: " + " -> ".join(active[active.index(task_id):] + [task_id]))
            return
        if task_id in done or task_id not in tasks:
            return
        active.append(task_id)
        for dependency in tasks[task_id]["depends_on"]:
            visit(dependency)
        active.pop()
        done.add(task_id)

    for task_id in tasks:
        visit(task_id)
    release_closure = set()

    def collect(task_id: str) -> None:
        if task_id in release_closure or task_id not in tasks:
            return
        release_closure.add(task_id)
        for dependency in tasks[task_id]["depends_on"]:
            collect(dependency)

    collect("A-08")
    for task_id, row in tasks.items():
        if row.get("required") and task_id not in release_closure:
            errors.append(f"{task_id}: required task is outside release prerequisite closure")
        if not row.get("required") and task_id in release_closure:
            errors.append(f"{task_id}: research task unexpectedly blocks required release")
    return errors


def prose(text: str) -> str:
    return re.sub(r"^```[^\n]*\n.*?^```\s*$", "", text, flags=re.M | re.S)


def anchors(path: Path) -> set[str]:
    result, counts = set(), Counter()
    for title in re.findall(r"^#{1,6}\s+(.+)$", prose(path.read_text()), re.M):
        slug = re.sub(r"[^\w\s-]", "", title.strip().lower())
        slug = re.sub(r"\s", "-", slug)
        count = counts[slug]
        counts[slug] += 1
        result.add(slug if count == 0 else f"{slug}-{count}")
    return result


def validate_files(root: Path, data: dict) -> list[str]:
    errors, anchor_cache = [], {}
    files = list(root.glob("*.md")) + [root / "plan.toml", root / "tools/check_plan.py"]
    for path in files:
        text = path.read_text()
        if not text.endswith("\n"):
            errors.append(f"{path.name}: missing final newline")
        if any(line.endswith((" ", "\t")) for line in text.splitlines()):
            errors.append(f"{path.name}: trailing whitespace")
        if path.suffix != ".md":
            continue
        for target in re.findall(r"\[[^\]]+\]\(([^)]+)\)", prose(text)):
            link = urlsplit(target)
            if link.scheme or link.netloc:
                continue
            destination = (path.parent / unquote(link.path)).resolve() if link.path else path
            if not destination.exists():
                errors.append(f"{path.name}: missing link {target}")
            elif link.fragment and destination.suffix == ".md":
                if destination not in anchor_cache:
                    anchor_cache[destination] = anchors(destination)
                if unquote(link.fragment) not in anchor_cache[destination]:
                    errors.append(f"{path.name}: missing anchor {target}")
    for row in data.get("tasks", []) + data.get("decisions", []):
        for ref in row.get("evidence", []):
            if not urlsplit(ref).scheme and not (root / ref).exists():
                errors.append(f"{row['id']}: missing local evidence reference {ref}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ready", action="store_true", help="show tasks whose recorded prerequisites are satisfied")
    parser.add_argument(
        "--json",
        action="store_true",
        help="emit one machine-readable summary instead of prose (validation errors remain on stderr)",
    )
    args = parser.parse_args()
    try:
        expected, decision_ids = definitions(ROOT)
        data = tomllib.loads((ROOT / "plan.toml").read_text())
        errors = validate_manifest(data, expected, decision_ids)
        if not errors:
            errors.extend(validate_files(ROOT, data))
    except (OSError, ValueError, TypeError) as error:
        print(f"计划结构错误：{error}", file=sys.stderr)
        return 1
    if errors:
        if args.json:
            print(json.dumps({
                "schema_version": 1,
                "valid": False,
                "plan": str(ROOT / "plan.toml"),
                "errors": errors,
                "scope": "planning-structure-only",
            }, ensure_ascii=False, sort_keys=True))
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    tasks = {row["id"]: row for row in data["tasks"]}
    decisions = {row["id"]: row for row in data["decisions"]}
    edges = sum(len(row["depends_on"]) for row in tasks.values())
    required = sum(row["required"] for row in tasks.values())
    accepted = sum(row["status"] == "已验收" for row in tasks.values())
    ready = [key for key, row in tasks.items()
             if row["status"] in {"未开始", "设计中"}
             and all(tasks[dep]["status"] == "已验收" for dep in row["depends_on"])
             and all(decisions[dep]["status"] in ACCEPTED_DESIGNS for dep in row["decision_dependencies"])]
    plan_path = ROOT / "plan.toml"
    summary = {
        "schema_version": 1,
        "valid": True,
        "plan": str(plan_path),
        "plan_sha256": hashlib.sha256(plan_path.read_bytes()).hexdigest(),
        "task_count": len(tasks),
        "required_task_count": required,
        "accepted_task_count": accepted,
        "decision_count": len(decisions),
        "task_dependency_count": edges,
        "ready_task_ids": ready,
        "task_evidence_count": sum(len(row.get("evidence", [])) for row in tasks.values()),
        "decision_evidence_count": sum(len(row.get("evidence", [])) for row in decisions.values()),
        "tasks_without_evidence": sorted(key for key, row in tasks.items() if not row.get("evidence")),
        "decisions_without_evidence": sorted(key for key, row in decisions.items() if not row.get("evidence")),
        "task_status_counts": dict(sorted(Counter(row["status"] for row in tasks.values()).items())),
        "decision_status_counts": dict(sorted(Counter(row["status"] for row in decisions.values()).items())),
        "scope": "planning-structure-only",
    }
    if args.json:
        print(json.dumps(summary, ensure_ascii=False, sort_keys=True))
        return 0
    print(f"计划结构通过：{len(tasks)} 任务（{required} 必需），{len(decisions)} 决策，{edges} 条任务依赖；已验收 {accepted}。")
    print("仅结构校验；未运行 Mimi 或验证发布证据。")
    if args.ready:
        print("台账前置满足：" + ("、".join(ready) or "无"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
