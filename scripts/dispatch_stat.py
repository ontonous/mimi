#!/usr/bin/env python3
"""Resolved/legacy dispatch 度量门禁（0.34.40, AF-4 前置 1）。

用法：
  scripts/dispatch_stat.py generate   # 跑基线语料，生成基线 JSON 入仓
  scripts/dispatch_stat.py check       # 跑基线语料，与基线对比，禁静默回退率上升
  scripts/dispatch_stat.py check --zero # 同时要求每个程序 legacy_fallback == 0
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


def mimi_binary() -> Path:
    env = os.environ.get("MIMI")
    if env:
        return Path(env)
    return ROOT / "target" / "debug" / "mimi"


def compile_with_stat(
    src: Path, out_dir: Path, tmpdir: Path, reachable: bool = False
) -> dict | None:
    """对单个源文件跑 MIMI_STAT=1 编译，返回 DispatchStats JSON 或 None（编译失败）。

    tmpdir 提供给 mimi build 的 std::env::temp_dir()（sandbox 下 /tmp 可能只读）。
    """
    env = dict(os.environ)
    env["MIMI_STAT"] = "1"
    if reachable:
        env["MIMI_REACHABLE_DISPATCH"] = "1"
    env["MIMI_STAT_OUT"] = str(out_dir)
    env["TMPDIR"] = str(tmpdir)
    out_bin = out_dir / "out"
    proc = subprocess.run(
        [str(mimi_binary()), "build", str(src), "-o", str(out_bin)],
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
    )
    # 编译失败（类型错误等）→ 无 stats JSON，跳过该程序。
    if proc.returncode != 0:
        return None
    # 读取 out_dir 下唯一的 src-*.json。
    jsons = list(out_dir.glob("src-*.json"))
    if not jsons:
        return None
    try:
        stats = json.loads(jsons[0].read_text())
    except (json.JSONDecodeError, OSError):
        return None
    # 用源文件相对路径覆盖 program 标识（codegen 层只有 source_id）。
    try:
        rel = src.relative_to(ROOT)
    except ValueError:
        rel = src
    stats["program"] = str(rel)
    return stats


def collect_all(reachable: bool = False) -> dict:
    """跑全部基线语料，返回 {程序相对路径: stats}。

    reachable=True 时设置 MIMI_REACHABLE_DISPATCH=1，度量仅含从入口可达的函数。
    """
    results: dict[str, dict] = {}
    bin_path = mimi_binary()
    if not bin_path.exists():
        print(f"[dispatch-stat] mimi 二进制不存在：{bin_path}（先 cargo build）", file=sys.stderr)
        sys.exit(2)
    files = corpus()
    print(f"[dispatch-stat] 语料 {len(files)} 个 .mimi 文件", file=sys.stderr)
    skipped = 0
    # 工作区内临时目录（sandbox 下 /tmp 可能只读）。
    tmp_root = ROOT / "target" / "dispatch-stat-tmp"
    if tmp_root.exists():
        # 0.34.45：开头清理同样容错（上次运行中断残留时 rmtree 可能
        # 在子目录上失败——Linux _rmtree_safe_fd 的 os.rmdir 竞态）。
        shutil.rmtree(tmp_root, ignore_errors=True)
    tmp_root.mkdir(parents=True, exist_ok=True)
    # mimi build 的 temp_dir()（TMPDIR）也需指向工作区可写目录。
    build_tmpdir = tmp_root / "build-tmp"
    build_tmpdir.mkdir()
    try:
        for i, src in enumerate(files, 1):
            try:
                rel = str(src.relative_to(ROOT))
            except ValueError:
                rel = str(src)
            out_dir = tmp_root / f"prog-{i}"
            out_dir.mkdir()
            try:
                stats = compile_with_stat(src, out_dir, build_tmpdir, reachable=reachable)
            except subprocess.TimeoutExpired:
                print(f"  [skip-timeout] {rel}", file=sys.stderr)
                skipped += 1
                continue
            if stats is None:
                skipped += 1
                continue
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
    print(f"[dispatch-stat] 完成：{len(results)} 成功 / {skipped} 跳过", file=sys.stderr)
    return results


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


def cmd_generate() -> int:
    results = collect_all()
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
    results = collect_all(reachable=reachable)
    doc = build_baseline_doc(results)
    print(json.dumps(doc, indent=2, ensure_ascii=False))
    return 0


def cmd_check(args: list[str]) -> int:
    reachable = "--reachable" in args
    require_zero = "--zero" in args
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
    results = collect_all(reachable=reachable)
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

    ok = True
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
    if cmd == "generate":
        return cmd_generate()
    if cmd == "report":
        return cmd_report(sys.argv[2:])
    if cmd == "check":
        return cmd_check(sys.argv[2:])
    if cmd == "sample":
        return cmd_sample(sys.argv[2:])
    return cmd_classify(sys.argv[2:])


if __name__ == "__main__":
    sys.exit(main())
