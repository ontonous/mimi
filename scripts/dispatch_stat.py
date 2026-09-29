#!/usr/bin/env python3
"""Resolved/legacy dispatch 度量门禁（0.34.40, AF-4 前置 1）。

用法：
  scripts/dispatch_stat.py generate   # 跑基线语料，生成基线 JSON 入仓
  scripts/dispatch_stat.py check       # 跑基线语料，与基线对比，禁静默回退率上升
  scripts/dispatch_stat.py check --zero # 同时要求每个程序 legacy_fallback == 0
  scripts/dispatch_stat.py check --core # 只扫核语料（Flow / spawn fixtures）
  scripts/dispatch_stat.py check --outcomes-json FILE
                                      # 保存每个输入的成功/不可用状态和诊断
  scripts/dispatch_stat.py report --outcomes-json FILE
                                      # 报告模式也可保存同一逐项清单
  scripts/dispatch_stat.py report     # 打印当前语料 fallback 报告（不对比）
  scripts/dispatch_stat.py classify [baseline.json] [--output FILE]
                                      # 对基线 skip_reasons 做根因分类，生成清单
  scripts/dispatch_stat.py sample [--limit N] [--program FILE] [--output FILE]
                                      # 跑 MIMI_VERBOSE=1，解析高频 resolved-skips

基线语料 = demos/*.mimi + examples/*.mimi + tests/real_world/*.mimi + projects/mimi-taskq|mimi-ledger/src/*.mimi。
每个程序以 MIMI_STAT=1 独立 MIMI_STAT_OUT 目录编译，读取 DispatchStats JSON，
以源文件名关联。

门禁规则（check 模式）：
  - 某程序 fallback_rate 相对基线上升 > EPSILON 且不在白名单 → 失败
  - 白名单登记制（同 ignored 测试纪律）：devdocs/v0.34/golden/dispatch-whitelist.json
  - 白名单条目必须带 reason；缺 reason 视为违规
  - 新增程序（基线没有）自动纳入，回退率记为当前值

classify 模式（0.37 Phase 0）：
  - 不重新编译语料，直接消费现有 dispatch-baseline.json
  - 将每个 skip_reasons 细化为根因大类（generics/qualified、
    module/source_id、unsupported_type、unsupported_expression、
    match_pattern、other）
  - 默认写入 devdocs/v0.37/dispatch-fallback-root-causes.json

环境变量：
  MIMI          — mimi 二进制路径（默认 ./target/debug/mimi）
  LLVM_SYS_181_PREFIX — LLVM wrapper 前缀（透传）
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASELINE_PATH = ROOT / "devdocs/v0.34/golden/dispatch-baseline.json"
WHITELIST_PATH = ROOT / "devdocs/v0.34/golden/dispatch-whitelist.json"
CLASSIFY_OUTPUT_PATH = ROOT / "devdocs/v0.37/dispatch-fallback-root-causes.json"
EPSILON = 1e-9  # 回退率上升超过此阈值即视为回退

# 根因分类（0.37 Phase 0：Legacy fallback 精确分类清单）。
CATEGORY_LABELS: dict[str, str] = {
    "generics/qualified": "泛型/限定名（generics/qualified）",
    "module/source_id": "模块函数体 source_id 未对齐（module file）",
    "unsupported_type": "不支持的类型（unsupported type / nominal type）",
    "unsupported_expression": "不支持的表达式（unsupported expression）",
    "match_pattern": "模式匹配边界（match pattern）",
    "other": "其他/待细分（other）",
}


def classify_reason(reason: str) -> str:
    """把单个 skip_reasons 文本映射到根因大类。

    顺序敏感：先识别明确的 generics/type 关键字，再检查表达式/模式边界。
    """
    r = reason.lower()
    if "generic" in r or "qualified" in r:
        return "generics/qualified"
    if "module file" in r or "source_id mismatch" in r:
        return "module/source_id"
    if (
        "unsupported type" in r
        or "nominal type" in r
        or "not a record or enum in the resolved native slice" in r
        or "not in the resolved native slice" in r
        or "type nothing" in r
    ):
        return "unsupported_type"
    if "unsupported expression" in r or "unmet expression backend requirement" in r:
        return "unsupported_expression"
    if "only literal" in r or "only value bindings" in r or "pattern" in r or "match" in r:
        return "match_pattern"
    return "other"


def corpus() -> list[Path]:
    """基线语料：demos + examples + tests/real_world + 0.1.7 dogfood/回归工程。"""
    files: list[Path] = []
    for d in (
        ROOT / "demos",
        ROOT / "examples",
        ROOT / "tests" / "real_world",
        ROOT / "projects" / "mimi-taskq" / "src",
        ROOT / "projects" / "mimi-ledger" / "src",
        ROOT / "projects" / "mimichat" / "src",
        ROOT / "projects" / "mimichat-modern" / "src",
    ):
        if d.is_dir():
            files.extend(sorted(d.rglob("*.mimi")))
    return files


def core_corpus() -> list[Path]:
    """0.1.8 Phase 0 核语料：一条核 Flow + 一条 spawn+channel。"""
    files = [
        ROOT / "tests" / "fixtures" / "core_flow_counter.mimi",
        ROOT / "tests" / "fixtures" / "spawn_channel_pingpong.mimi",
    ]
    return [p for p in files if p.is_file()]


def mimi_binary() -> Path:
    env = os.environ.get("MIMI")
    if env:
        return Path(env)
    return ROOT / "target" / "debug" / "mimi"


def _captured_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


def _diagnostic_excerpt(stdout: str | bytes | None, stderr: str | bytes | None) -> str:
    parts: list[str] = []
    for label, captured in (("stderr", stderr), ("stdout", stdout)):
        text = _captured_text(captured).strip()
        if text:
            lines = text.splitlines()[-8:]
            parts.append(f"{label}: " + "\n".join(lines))
    excerpt = "\n".join(parts)
    if len(excerpt) > 2400:
        excerpt = "…" + excerpt[-2399:]
    return excerpt


def _route_observation(stdout: str | bytes | None, stderr: str | bytes | None) -> dict:
    captured = "\n".join((_captured_text(stderr), _captured_text(stdout)))
    match = re.search(
        r"canonical route disposition: (canonical|legacy) \(([^)]+)\)(?: mir_digest=([0-9a-f]{64}))?",
        captured,
    )
    if match:
        return {
            "route_disposition": match.group(1),
            "route_reason": match.group(2),
            "mir_digest": match.group(3),
        }
    if "default Canonical MIR route rejected:" in captured:
        return {
            "route_disposition": "rejected",
            "route_reason": "default-canonical-mir-route-rejected",
            "mir_digest": None,
        }
    return {"route_disposition": "unobserved", "route_reason": None, "mir_digest": None}


def _outcome(
    src: Path,
    status: str,
    started_at: float,
    *,
    exit_code: int | None = None,
    diagnostic: str = "",
    stats_file: str | None = None,
    route: dict | None = None,
) -> dict:
    try:
        program = str(src.relative_to(ROOT))
    except ValueError:
        program = str(src)
    result = {
        "program": program,
        "status": status,
        "phase": "dispatch-stats" if status.startswith("stats_") else "build",
        "exit_code": exit_code,
        "elapsed_ms": round((time.monotonic() - started_at) * 1000, 3),
        "stats_file": stats_file,
        "diagnostic": diagnostic,
        "dispatch_summary": None,
    }
    result.update(route or {"route_disposition": "unobserved", "route_reason": None, "mir_digest": None})
    return result


def compile_with_stat(
    src: Path, out_dir: Path, tmpdir: Path, reachable: bool = False
) -> tuple[dict | None, dict]:
    """对单个源文件运行 MIMI_STAT=1，并保留每种不可用结果及诊断。

    tmpdir 提供给 mimi build 的 std::env::temp_dir()（sandbox 下 /tmp 可能只读）。
    """
    env = dict(os.environ)
    env["MIMI_STAT"] = "1"
    env["MIMI_VERBOSE"] = "1"
    env.pop("MIMI_REACHABLE_DISPATCH", None)
    if reachable:
        env["MIMI_REACHABLE_DISPATCH"] = "1"
    env["MIMI_STAT_OUT"] = str(out_dir)
    env["TMPDIR"] = str(tmpdir)
    out_bin = out_dir / "out"
    started_at = time.monotonic()
    try:
        proc = subprocess.run(
            [str(mimi_binary()), "build", str(src), "-o", str(out_bin)],
            capture_output=True,
            text=True,
            errors="replace",
            env=env,
            timeout=120,
        )
    except subprocess.TimeoutExpired as error:
        return None, _outcome(
            src,
            "timeout",
            started_at,
            diagnostic=_diagnostic_excerpt(error.stdout, error.stderr),
            route=_route_observation(error.stdout, error.stderr),
        )
    except OSError as error:
        return None, _outcome(src, "spawn_error", started_at, diagnostic=str(error))

    if proc.returncode != 0:
        return None, _outcome(
            src,
            "build_failed",
            started_at,
            exit_code=proc.returncode,
            diagnostic=_diagnostic_excerpt(proc.stdout, proc.stderr),
            route=_route_observation(proc.stdout, proc.stderr),
        )

    jsons = list(out_dir.glob("src-*.json"))
    if not jsons:
        return None, _outcome(
            src,
            "stats_missing",
            started_at,
            exit_code=proc.returncode,
            diagnostic=_diagnostic_excerpt(proc.stdout, proc.stderr),
            route=_route_observation(proc.stdout, proc.stderr),
        )
    if len(jsons) != 1:
        return None, _outcome(
            src,
            "stats_ambiguous",
            started_at,
            exit_code=proc.returncode,
            diagnostic=f"expected exactly one src-*.json file, found {len(jsons)}",
            route=_route_observation(proc.stdout, proc.stderr),
        )

    try:
        stats = json.loads(jsons[0].read_text(encoding="utf-8"))
    except OSError as error:
        return None, _outcome(
            src,
            "stats_unreadable",
            started_at,
            exit_code=proc.returncode,
            diagnostic=str(error),
            stats_file=jsons[0].name,
            route=_route_observation(proc.stdout, proc.stderr),
        )
    except UnicodeDecodeError as error:
        return None, _outcome(
            src,
            "stats_invalid_encoding",
            started_at,
            exit_code=proc.returncode,
            diagnostic=str(error),
            stats_file=jsons[0].name,
            route=_route_observation(proc.stdout, proc.stderr),
        )
    except json.JSONDecodeError as error:
        return None, _outcome(
            src,
            "stats_invalid_json",
            started_at,
            exit_code=proc.returncode,
            diagnostic=f"{error.msg} at line {error.lineno}, column {error.colno}",
            stats_file=jsons[0].name,
            route=_route_observation(proc.stdout, proc.stderr),
        )
    if not isinstance(stats, dict):
        return None, _outcome(
            src,
            "stats_invalid_shape",
            started_at,
            exit_code=proc.returncode,
            diagnostic=f"expected JSON object, got {type(stats).__name__}",
            stats_file=jsons[0].name,
            route=_route_observation(proc.stdout, proc.stderr),
        )

    outcome = _outcome(
        src,
        "stats_available",
        started_at,
        exit_code=proc.returncode,
        stats_file=jsons[0].name,
        route=_route_observation(proc.stdout, proc.stderr),
    )
    outcome["dispatch_summary"] = {
        field: int(stats.get(field, 0))
        for field in ("total_functions", "eligible", "legacy_fallback", "emit_failed")
    }
    stats["program"] = outcome["program"]
    return stats, outcome


def _write_outcomes(path: Path, outcomes: list[dict], input_count: int) -> None:
    counts: dict[str, int] = {}
    for item in outcomes:
        counts[item["status"]] = counts.get(item["status"], 0) + 1
    route_counts: dict[str, int] = {}
    for item in outcomes:
        route = item.get("route_disposition", "unobserved")
        route_counts[route] = route_counts.get(route, 0) + 1
    dispatch_totals = {
        "instrumented_programs": 0,
        "total_functions": 0,
        "eligible": 0,
        "legacy_fallback": 0,
        "emit_failed": 0,
    }
    for item in outcomes:
        summary = item.get("dispatch_summary")
        if summary is None:
            continue
        dispatch_totals["instrumented_programs"] += 1
        for field in ("total_functions", "eligible", "legacy_fallback", "emit_failed"):
            dispatch_totals[field] += int(summary.get(field, 0))
    report = {
        "schema_version": "0.41-dispatch-probe-outcomes-v3",
        "input_count": input_count,
        "outcome_count": len(outcomes),
        "status_counts": dict(sorted(counts.items())),
        "route_counts": dict(sorted(route_counts.items())),
        "dispatch_totals": dispatch_totals,
        "outcomes": outcomes,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as stream:
        stream.write(json.dumps(report, indent=2, ensure_ascii=False) + "\n")


def _outcome_path_arg(args: list[str]) -> Path | None:
    for index, arg in enumerate(args):
        if arg.startswith("--outcomes-json="):
            raw_path = arg.split("=", 1)[1]
            if raw_path:
                path = Path(raw_path)
                return path if path.is_absolute() else ROOT / path
            raise ValueError("--outcomes-json requires a file path")
        if arg == "--outcomes-json":
            if index + 1 >= len(args) or args[index + 1].startswith("--"):
                raise ValueError("--outcomes-json requires a file path")
            path = Path(args[index + 1])
            return path if path.is_absolute() else ROOT / path
    return None


def collect_all(
    reachable: bool = False,
    core: bool = False,
    outcomes_json: Path | None = None,
) -> tuple[dict, list[dict]]:
    """跑全部语料，返回 (可用统计, 每输入结果清单)。

    reachable=True 时设置 MIMI_REACHABLE_DISPATCH=1，度量仅含从入口可达的函数。
    core=True 时只扫 0.1.8 Phase 0 核语料（Flow / spawn fixtures）。
    """
    results: dict[str, dict] = {}
    bin_path = mimi_binary()
    if not bin_path.exists():
        print(f"[dispatch-stat] mimi 二进制不存在：{bin_path}（先 cargo build）", file=sys.stderr)
        sys.exit(2)
    if outcomes_json is not None and outcomes_json.exists():
        print(
            f"[dispatch-stat] refusing to overwrite outcome file: {outcomes_json}",
            file=sys.stderr,
        )
        sys.exit(2)
    files = core_corpus() if core else corpus()
    print(f"[dispatch-stat] 语料 {len(files)} 个 .mimi 文件", file=sys.stderr)
    target_dir = ROOT / "target"
    target_dir.mkdir(parents=True, exist_ok=True)
    # Every run owns a fresh path. Never delete a fixed shared directory that
    # may contain artifacts from an interrupted or concurrent invocation.
    tmp_root = Path(tempfile.mkdtemp(prefix="dispatch-stat-", dir=target_dir))
    outcomes: list[dict] = []
    try:
        # mimi build 的 temp_dir()（TMPDIR）也需指向本次调用拥有的目录。
        build_tmpdir = tmp_root / "build-tmp"
        build_tmpdir.mkdir()
        for i, src in enumerate(files, 1):
            out_dir = tmp_root / f"prog-{i}"
            out_dir.mkdir()
            stats, result = compile_with_stat(src, out_dir, build_tmpdir, reachable=reachable)
            outcomes.append(result)
            if stats is None:
                print(
                    f"  [unavailable:{result['status']}] {result['program']} "
                    f"exit={result['exit_code']} elapsed_ms={result['elapsed_ms']:.1f}",
                    file=sys.stderr,
                )
                if result["diagnostic"]:
                    print(f"    {result['diagnostic']}", file=sys.stderr)
                continue
            rel = result["program"]
            results[rel] = stats
            rate = stats.get("fallback_rate")
            if rate is None:
                total = stats.get("total_functions", 0)
                legacy = stats.get("legacy_fallback", 0)
                rate = 1.0 if total == 0 else legacy / total
            print(
                f"  [{i}/{len(files)}] {rel}: eligible={stats.get('eligible', 0)}/"
                f"{stats.get('total_functions', 0)} fallback={rate:.3f}",
                file=sys.stderr,
            )
    finally:
        shutil.rmtree(tmp_root, ignore_errors=True)
    counts: dict[str, int] = {}
    for result in outcomes:
        counts[result["status"]] = counts.get(result["status"], 0) + 1
    print(
        f"[dispatch-stat] 完成：{counts.get('stats_available', 0)} 生成统计 / "
        f"{len(outcomes) - counts.get('stats_available', 0)} 结果不可用；"
        f"status={json.dumps(dict(sorted(counts.items())), ensure_ascii=False)}",
        file=sys.stderr,
    )
    if outcomes_json is not None:
        _write_outcomes(outcomes_json, outcomes, len(files))
        print(f"[dispatch-stat] per-input outcomes: {outcomes_json}", file=sys.stderr)
    return results, outcomes


def build_baseline_doc(results: dict) -> dict:
    """组装基线文档（含聚合）。"""
    total_fn = sum(s.get("total_functions", 0) for s in results.values())
    total_eligible = sum(s.get("eligible", 0) for s in results.values())
    total_legacy = sum(s.get("legacy_fallback", 0) for s in results.values())
    agg_rate = 1.0 if total_fn == 0 else total_legacy / total_fn
    programs = {}
    for name, s in sorted(results.items()):
        tf = s.get("total_functions", 0)
        lg = s.get("legacy_fallback", 0)
        programs[name] = {
            "total_functions": tf,
            "eligible": s.get("eligible", 0),
            "legacy_fallback": lg,
            "emit_failed": s.get("emit_failed", 0),
            "fallback_rate": (1.0 if tf == 0 else lg / tf),
            "skip_reasons": s.get("skip_reasons", {}),
        }
    return {
        "baseline_version": "0.37.0",
        "corpus": "demos/ + examples/ + tests/real_world/ + 0.1.7 dogfood projects/",
        "aggregate": {
            "total_functions": total_fn,
            "eligible": total_eligible,
            "legacy_fallback": total_legacy,
            "fallback_rate": agg_rate,
        },
        "programs": programs,
    }


def build_fallback_classification(
    baseline: dict, baseline_path: Path | None = None
) -> dict:
    """把 baseline.programs[].skip_reasons 聚合成精确根因分类清单。

    该命令不重新编译语料；输入即 dispatch-baseline.json，输出同时包含：
      - aggregate: 全语料根因大类计数
      - reasons:   每个原始 skip_reason 的计数与所属大类
      - programs:  每个程序的分类明细（供 Phase A 按程序驱动攻坚）
    """
    from collections import Counter

    program_models: dict[str, dict] = {}
    aggregate_categories: Counter[str] = Counter()
    per_reason: Counter[str] = Counter()

    for name, info in baseline.get("programs", {}).items():
        reason_counts = info.get("skip_reasons", {})
        if not isinstance(reason_counts, dict):
            continue
        categories: Counter[str] = Counter()
        reason_models: dict[str, dict] = {}
        total_skipped = 0
        for reason, count in reason_counts.items():
            cat = classify_reason(reason)
            categories[cat] += count
            aggregate_categories[cat] += count
            per_reason[reason] += count
            total_skipped += count
            reason_models[reason] = {"count": count, "category": cat}
        program_models[name] = {
            "total_functions": info.get("total_functions", 0),
            "eligible": info.get("eligible", 0),
            "legacy_fallback": info.get("legacy_fallback", 0),
            "fallback_rate": info.get("fallback_rate", 0),
            "skip_reason_count": total_skipped,
            "categories": dict(sorted(categories.items(), key=lambda kv: (-kv[1], kv[0]))),
            "reasons": reason_models,
        }

    total_fallback = sum(aggregate_categories.values())
    baseline_ref = baseline_path if baseline_path is not None else BASELINE_PATH
    try:
        baseline_file_display = str(baseline_ref.relative_to(ROOT))
    except ValueError:
        baseline_file_display = str(baseline_ref)
    return {
        "schema_version": "0.37-classify-1",
        "baseline_file": baseline_file_display,
        "legacy_fallback_total": total_fallback,
        "aggregate": {
            "categories": dict(
                sorted(aggregate_categories.items(), key=lambda kv: (-kv[1], kv[0]))
            ),
            "reasons": dict(
                sorted(per_reason.items(), key=lambda kv: (-kv[1], kv[0]))
            ),
        },
        "programs": program_models,
    }


VERBOSE_SKIP_RE = re.compile(r"^info: resolved skip '([^']+)': (.*)$", re.MULTILINE)


def collect_verbose_skips(
    src: Path, out_dir: Path, build_tmpdir: Path, reachable: bool = False
) -> list[tuple[str, str]]:
    """对单个程序跑 MIMI_VERBOSE=1，返回 (function_display_name, reason) 列表。

    reachable=True 时同时设置 MIMI_REACHABLE_DISPATCH=1，只看可达函数的 skip。
    """
    env = dict(os.environ)
    env["MIMI_VERBOSE"] = "1"
    if reachable:
        env["MIMI_REACHABLE_DISPATCH"] = "1"
    env["TMPDIR"] = str(build_tmpdir)
    out_bin = out_dir / "out"
    proc = subprocess.run(
        [str(mimi_binary()), "build", str(src), "-o", str(out_bin)],
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
    )
    if proc.returncode != 0:
        return []
    return [
        (m.group(1), m.group(2).strip())
        for m in VERBOSE_SKIP_RE.finditer(proc.stderr)
    ]


def cmd_sample(args: list[str]) -> int:
    """sample [--limit N] [--program FILE] [--output FILE] [--reachable]

    跑 MIMI_VERBOSE=1 采样，解析 `resolved skip '<name>': reason`，
    输出高频 skip 函数名与原因。默认全语料；可用 --limit 限制数量；
    --reachable 时同时启用仅统计可达函数的实验路径。
    """
    limit: int | None = None
    program: Path | None = None
    output_path: Path | None = None
    reachable = False
    i = 0
    while i < len(args):
        if args[i] == "--reachable":
            reachable = True
            i += 1
        elif args[i] == "--limit":
            if i + 1 >= len(args):
                print("[dispatch-stat] sample: --limit 需要数字", file=sys.stderr)
                return 2
            limit = int(args[i + 1])
            i += 2
        elif args[i] == "--program":
            if i + 1 >= len(args):
                print("[dispatch-stat] sample: --program 需要路径", file=sys.stderr)
                return 2
            program = Path(args[i + 1])
            i += 2
        elif args[i] == "--output":
            if i + 1 >= len(args):
                print("[dispatch-stat] sample: --output 需要文件路径", file=sys.stderr)
                return 2
            output_path = Path(args[i + 1])
            i += 2
        else:
            print(f"[dispatch-stat] sample: 未知参数 {args[i]}", file=sys.stderr)
            return 2

    bin_path = mimi_binary()
    if not bin_path.exists():
        print(f"[dispatch-stat] sample: mimi 二进制不存在：{bin_path}（先 cargo build）", file=sys.stderr)
        return 2

    files = [program] if program is not None else corpus()
    if limit is not None:
        files = files[:limit]

    tmp_root = ROOT / "target" / "dispatch-sample-tmp"
    if tmp_root.exists():
        shutil.rmtree(tmp_root, ignore_errors=True)
    tmp_root.mkdir(parents=True, exist_ok=True)
    build_tmpdir = tmp_root / "build-tmp"
    build_tmpdir.mkdir()

    from collections import Counter

    by_name: Counter[str] = Counter()
    by_reason: Counter[str] = Counter()
    by_pair: Counter[tuple[str, str]] = Counter()
    program_samples: dict[str, list[dict]] = {}

    try:
        for index, src in enumerate(files, 1):
            rel = str(src.relative_to(ROOT)) if src.is_relative_to(ROOT) else str(src)
            out_dir = tmp_root / f"prog-{index}"
            out_dir.mkdir()
            skips = collect_verbose_skips(src, out_dir, build_tmpdir, reachable=reachable)
            sample_models: list[dict] = []
            for name, reason in skips:
                by_name[name] += 1
                by_reason[reason] += 1
                by_pair[(name, reason)] += 1
                sample_models.append({"name": name, "reason": reason})
            program_samples[rel] = sample_models
            print(
                f"  [{index}/{len(files)}] {rel}: {len(skips)} resolved-skips",
                file=sys.stderr,
            )
    finally:
        shutil.rmtree(tmp_root, ignore_errors=True)

    doc = {
        "schema_version": "0.37-sample-1",
        "limit": limit,
        "program": str(program) if program is not None else None,
        "programs": program_samples,
        "aggregate": {
            "by_name": dict(by_name.most_common()),
            "by_reason": dict(by_reason.most_common()),
            "by_pair": [
                {"name": name, "reason": reason, "count": count}
                for (name, reason), count in by_pair.most_common()
            ],
        },
    }

    if output_path is not None:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n")
        try:
            display_output = output_path.relative_to(ROOT)
        except ValueError:
            display_output = output_path
        print(f"[dispatch-stat] sample: 已写入 {display_output}", file=sys.stderr)

    print(f"[dispatch-stat] sample: 共采样 {len(files)} 个程序，{sum(by_pair.values())} 个 resolved-skips")
    print("\nTop 30 函数名（按出现程序/次数）：")
    for name, count in by_name.most_common(30):
        print(f"  {count:5d}  {name}")
    print("\nTop 30 skip 原因（按出现次数）：")
    for reason, count in by_reason.most_common(30):
        print(f"  {count:5d}  {reason}")
    return 0


def cmd_classify(args: list[str]) -> int:
    """classify [baseline.json] [--output FILE] [--check]"""
    baseline_path = BASELINE_PATH
    output_path = CLASSIFY_OUTPUT_PATH
    check_mode = False
    i = 0
    while i < len(args):
        if args[i] == "--output":
            if i + 1 >= len(args):
                print("[dispatch-stat] classify: --output 需要文件路径", file=sys.stderr)
                return 2
            output_path = Path(args[i + 1])
            i += 2
        elif args[i] == "--check":
            check_mode = True
            i += 1
        elif args[i].startswith("--"):
            print(f"[dispatch-stat] classify: 未知参数 {args[i]}", file=sys.stderr)
            return 2
        else:
            baseline_path = Path(args[i])
            i += 1
    if not baseline_path.exists():
        print(
            f"[dispatch-stat] classify: 基线文件不存在：{baseline_path}（先 generate 或指定路径）",
            file=sys.stderr,
        )
        return 2
    try:
        baseline = json.loads(baseline_path.read_text())
    except json.JSONDecodeError as e:
        print(f"[dispatch-stat] classify: 基线 JSON 解析失败：{e}", file=sys.stderr)
        return 2
    if "programs" not in baseline:
        print("[dispatch-stat] classify: 输入不是 dispatch 基线 JSON（缺 programs）", file=sys.stderr)
        return 2

    classification = build_fallback_classification(baseline, baseline_path)
    if check_mode:
        if not output_path.exists():
            print(
                f"[dispatch-stat] classify --check: 清单不存在：{output_path}（先运行 classify 生成）",
                file=sys.stderr,
            )
            return 2
        try:
            existing = json.loads(output_path.read_text())
        except json.JSONDecodeError as e:
            print(f"[dispatch-stat] classify --check: 清单 JSON 解析失败：{e}", file=sys.stderr)
            return 2
        if existing == classification:
            print(f"[dispatch-stat] classify --check: ✅ {output_path} 与当前基线一致")
            return 0
        print(
            f"[dispatch-stat] classify --check: ❌ {output_path} 已过期，"
            "请运行 `scripts/dispatch_stat.py classify` 重新生成",
            file=sys.stderr,
        )
        return 1

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(classification, indent=2, ensure_ascii=False) + "\n")

    agg = classification["aggregate"]["categories"]
    try:
        display_output = output_path.relative_to(ROOT)
    except ValueError:
        display_output = output_path
    print(f"[dispatch-stat] classify: 根因分类已写入 {display_output}")
    print(f"[dispatch-stat] classify: legacy_fallback_total={classification['legacy_fallback_total']}")
    for cat, count in agg.items():
        label = CATEGORY_LABELS.get(cat, cat)
        print(f"  {count:5d}  {label} ({cat})")
    return 0


def load_whitelist() -> dict:
    if not WHITELIST_PATH.exists():
        return {}
    try:
        raw = json.loads(WHITELIST_PATH.read_text())
    except json.JSONDecodeError:
        print(f"[dispatch-stat] 白名单 JSON 解析失败：{WHITELIST_PATH}", file=sys.stderr)
        sys.exit(2)
    # 过滤 `_` 前缀的说明性 key（_doc/_example 等）。
    return {k: v for k, v in raw.items() if not k.startswith("_")}


def _unavailable_outcomes(outcomes: list[dict]) -> list[dict]:
    return [item for item in outcomes if item["status"] != "stats_available"]


def _report_outcome_summary(outcomes: list[dict]) -> dict:
    counts: dict[str, int] = {}
    for item in outcomes:
        counts[item["status"]] = counts.get(item["status"], 0) + 1
    return {
        "input_count": len(outcomes),
        "stats_available": counts.get("stats_available", 0),
        "unavailable": len(outcomes) - counts.get("stats_available", 0),
        "status_counts": dict(sorted(counts.items())),
    }


def _report_unavailable_gate(outcomes: list[dict]) -> bool:
    unavailable = _unavailable_outcomes(outcomes)
    if not unavailable:
        return False
    print(
        f"[dispatch-stat] ❌ {len(unavailable)} 个输入未生成可用 dispatch 统计；"
        "门禁结果不完整，逐项分类后再确认。",
        file=sys.stderr,
    )
    return True


def cmd_generate(args: list[str]) -> int:
    results, outcomes = collect_all(outcomes_json=_outcome_path_arg(args))
    if _report_unavailable_gate(outcomes):
        print("[dispatch-stat] 拒绝从不完整语料生成基线", file=sys.stderr)
        return 2
    if not results:
        print("[dispatch-stat] 无任何程序编译成功，拒绝生成空基线", file=sys.stderr)
        return 2
    doc = build_baseline_doc(results)
    BASELINE_PATH.parent.mkdir(parents=True, exist_ok=True)
    BASELINE_PATH.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n")
    agg = doc["aggregate"]
    print(
        f"[dispatch-stat] 基线已写入 {BASELINE_PATH.relative_to(ROOT)}："
        f"{agg['eligible']}/{agg['total_functions']} eligible，"
        f"fallback_rate={agg['fallback_rate']:.4f}"
    )
    return 0


def cmd_report(args: list[str]) -> int:
    reachable = "--reachable" in args
    results, outcomes = collect_all(
        reachable=reachable,
        outcomes_json=_outcome_path_arg(args),
    )
    doc = build_baseline_doc(results)
    doc["probe_outcomes"] = _report_outcome_summary(outcomes)
    print(json.dumps(doc, indent=2, ensure_ascii=False))
    return 0


def cmd_check(args: list[str]) -> int:
    reachable = "--reachable" in args
    require_zero = "--zero" in args
    core_only = "--core" in args
    outcomes_json = _outcome_path_arg(args)
    if core_only:
        results, outcomes = collect_all(
            reachable=reachable,
            core=True,
            outcomes_json=outcomes_json,
        )
        unavailable = _unavailable_outcomes(outcomes)
        if not results:
            print("[dispatch-stat] --core 核语料无任何程序编译成功", file=sys.stderr)
            return 2
        zero_violations: list[str] = [
            f"{item['program']}: dispatch stats unavailable ({item['status']})"
            for item in unavailable
        ]
        for name, s in sorted(results.items()):
            lg = s.get("legacy_fallback", 0)
            el = s.get("eligible", 0)
            print(
                f"  [core] {name}: eligible={el} legacy_fallback={lg} "
                f"emitter={'resolved' if el > 0 else 'none'}"
            )
            # 核函数门禁：程序必须走进 resolved（eligible>0），且不得静默
            # fallback。transition 基础设施可能仍记在 skip 里，所以 --core
            # 要求 eligible>0；--core --zero 才要求整程序 fallback==0。
            if el == 0:
                zero_violations.append(f"{name}: eligible=0（核程序必须 resolved）")
            if require_zero and lg > 0:
                zero_violations.append(f"{name}: legacy_fallback={lg}（--zero 要求 0）")
        if zero_violations:
            print("\n[dispatch-stat] ❌ 核语料门禁未满足：", file=sys.stderr)
            for v in zero_violations:
                print(f"    - {v}", file=sys.stderr)
            return 1
        print("[dispatch-stat] ✅ 核语料 emitter=resolved，无静默 core fallback")
        return 0
    if not BASELINE_PATH.exists():
        print(
            f"[dispatch-stat] 基线不存在：{BASELINE_PATH.relative_to(ROOT)}，"
            f"先跑 `scripts/dispatch_stat.py generate`",
            file=sys.stderr,
        )
        return 2
    baseline = json.loads(BASELINE_PATH.read_text())
    base_programs = baseline.get("programs", {})
    whitelist = load_whitelist()
    results, outcomes = collect_all(
        reachable=reachable,
        outcomes_json=outcomes_json,
    )
    incomplete = _report_unavailable_gate(outcomes)
    if not results:
        print("[dispatch-stat] 无任何程序编译成功", file=sys.stderr)
        return 2

    regressions: list[str] = []
    wl_violations: list[str] = []
    zero_violations: list[str] = []
    for name, s in sorted(results.items()):
        tf = s.get("total_functions", 0)
        lg = s.get("legacy_fallback", 0)
        cur_rate = 1.0 if tf == 0 else lg / tf
        if require_zero and lg > 0:
            zero_violations.append(
                f"{name}: legacy_fallback={lg}（--zero 要求 0）"
            )
        base = base_programs.get(name)
        if base is None:
            # 新程序：纳入基线，不回退判定（首次见）。
            print(f"  [new] {name}: fallback_rate={cur_rate:.4f}（首次纳入）", file=sys.stderr)
            continue
        base_rate = base.get("fallback_rate", 1.0)
        if cur_rate > base_rate + EPSILON:
            entry = whitelist.get(name)
            if entry is None:
                regressions.append(
                    f"{name}: {base_rate:.4f} → {cur_rate:.4f}（上升 {cur_rate - base_rate:+.4f}）"
                )
            else:
                reason = entry.get("reason", "").strip()
                if not reason:
                    wl_violations.append(f"{name}: 白名单条目缺 reason")
                else:
                    print(
                        f"  [whitelisted] {name}: {base_rate:.4f} → {cur_rate:.4f}"
                        f"（reason: {reason}）",
                        file=sys.stderr,
                    )

    ok = not incomplete
    if regressions:
        ok = False
        print("\n[dispatch-stat] ❌ 检测到静默回退率上升（未登记白名单）：", file=sys.stderr)
        for r in regressions:
            print(f"    - {r}", file=sys.stderr)
        print(
            "\n  处置：若为合法回退（新特性暂时只能 legacy），在 "
            f"{WHITELIST_PATH.relative_to(ROOT)} 登记该程序 + reason；"
            "否则修复 resolved emitter 覆盖。",
            file=sys.stderr,
        )
    if wl_violations:
        ok = False
        print("\n[dispatch-stat] ❌ 白名单违规：", file=sys.stderr)
        for v in wl_violations:
            print(f"    - {v}", file=sys.stderr)
    if zero_violations:
        ok = False
        print("\n[dispatch-stat] ❌ 零回退硬门禁未满足：", file=sys.stderr)
        for v in zero_violations:
            print(f"    - {v}", file=sys.stderr)

    if ok:
        agg_cur = build_baseline_doc(results)["aggregate"]
        agg_base = baseline.get("aggregate", {})
        print(
            f"[dispatch-stat] ✅ 无静默回退。当前聚合 fallback_rate="
            f"{agg_cur['fallback_rate']:.4f}（基线 {agg_base.get('fallback_rate', 0):.4f}）"
        )
        return 0
    return 1


def main() -> int:
    if len(sys.argv) < 2 or sys.argv[1] not in {"generate", "check", "report", "classify", "sample"}:
        print(__doc__, file=sys.stderr)
        return 2
    cmd = sys.argv[1]
    try:
        if cmd == "generate":
            return cmd_generate(sys.argv[2:])
        if cmd == "report":
            return cmd_report(sys.argv[2:])
        if cmd == "check":
            return cmd_check(sys.argv[2:])
        if cmd == "sample":
            return cmd_sample(sys.argv[2:])
        return cmd_classify(sys.argv[2:])
    except (OSError, ValueError) as error:
        print(f"[dispatch-stat] error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
