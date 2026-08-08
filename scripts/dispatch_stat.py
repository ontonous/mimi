#!/usr/bin/env python3
"""Resolved/legacy dispatch 度量门禁（0.34.40, AF-4 前置 1）。

用法：
  scripts/dispatch_stat.py generate   # 跑基线语料，生成基线 JSON 入仓
  scripts/dispatch_stat.py check      # 跑基线语料，与基线对比，禁静默回退率上升
  scripts/dispatch_stat.py report     # 打印当前语料 fallback 报告（不对比）

基线语料 = demos/*.mimi + examples/*.mimi + tests/real_world/*.mimi。
每个程序以 MIMI_STAT=1 独立 MIMI_STAT_OUT 目录编译，读取 DispatchStats JSON，
以源文件名关联。

门禁规则（check 模式）：
  - 某程序 fallback_rate 相对基线上升 > EPSILON 且不在白名单 → 失败
  - 白名单登记制（同 ignored 测试纪律）：devdocs/v0.34/golden/dispatch-whitelist.json
  - 白名单条目必须带 reason；缺 reason 视为违规
  - 新增程序（基线没有）自动纳入，回退率记为当前值

环境变量：
  MIMI          — mimi 二进制路径（默认 ./target/debug/mimi）
  LLVM_SYS_181_PREFIX — LLVM wrapper 前缀（透传）
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASELINE_PATH = ROOT / "devdocs/v0.34/golden/dispatch-baseline.json"
WHITELIST_PATH = ROOT / "devdocs/v0.34/golden/dispatch-whitelist.json"
EPSILON = 1e-9  # 回退率上升超过此阈值即视为回退


def corpus() -> list[Path]:
    """基线语料：demos + examples + tests/real_world 的 .mimi 文件。"""
    files: list[Path] = []
    for d in (ROOT / "demos", ROOT / "examples", ROOT / "tests" / "real_world"):
        if d.is_dir():
            files.extend(sorted(d.rglob("*.mimi")))
    return files


def mimi_binary() -> Path:
    env = os.environ.get("MIMI")
    if env:
        return Path(env)
    return ROOT / "target" / "debug" / "mimi"


def compile_with_stat(src: Path, out_dir: Path, tmpdir: Path) -> dict | None:
    """对单个源文件跑 MIMI_STAT=1 编译，返回 DispatchStats JSON 或 None（编译失败）。

    tmpdir 提供给 mimi build 的 std::env::temp_dir()（sandbox 下 /tmp 可能只读）。
    """
    env = dict(os.environ)
    env["MIMI_STAT"] = "1"
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


def collect_all() -> dict:
    """跑全部基线语料，返回 {程序相对路径: stats}。"""
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
                stats = compile_with_stat(src, out_dir, build_tmpdir)
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
        "baseline_version": "0.34.40",
        "corpus": "demos/ + examples/ + tests/real_world/",
        "aggregate": {
            "total_functions": total_fn,
            "eligible": total_eligible,
            "legacy_fallback": total_legacy,
            "fallback_rate": agg_rate,
        },
        "programs": programs,
    }


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


def cmd_report() -> int:
    results = collect_all()
    doc = build_baseline_doc(results)
    print(json.dumps(doc, indent=2, ensure_ascii=False))
    return 0


def cmd_check() -> int:
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
    results = collect_all()
    if not results:
        print("[dispatch-stat] 无任何程序编译成功", file=sys.stderr)
        return 2

    regressions: list[str] = []
    wl_violations: list[str] = []
    for name, s in sorted(results.items()):
        tf = s.get("total_functions", 0)
        lg = s.get("legacy_fallback", 0)
        cur_rate = 1.0 if tf == 0 else lg / tf
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
    if len(sys.argv) < 2 or sys.argv[1] not in {"generate", "check", "report"}:
        print(__doc__, file=sys.stderr)
        return 2
    cmd = sys.argv[1]
    if cmd == "generate":
        return cmd_generate()
    if cmd == "check":
        return cmd_check()
    return cmd_report()


if __name__ == "__main__":
    sys.exit(main())
