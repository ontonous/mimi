#!/usr/bin/env python3
"""Collect source-bound build/test evidence without overwriting earlier results.

This is a local evidence runner, not a semantic oracle or a B-09 sign-off.
Build with Cargo's --message-format=json; run uses its exact hashed executable.
"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import time


SCRIPT = Path(__file__).resolve()
INPUT_DIRS = ("src", "tests", "stdlib", "examples", "fixtures", "benches", "scripts", ".cargo")
ROOT_INPUTS = ("Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain", "rust-toolchain.toml")
SKIP_DIRS = {".git", "target", "node_modules", "__pycache__"}
OUTPUT_SUFFIXES = {".a", ".o", ".so", ".exe", ".bc", ".ll", ".pyc"}
MAX_SOURCE_INPUT_BYTES = 64 * 1024 * 1024
MAX_SOURCE_ARCHIVE_BYTES = 48 * 1024 * 1024
ENV_KEYS = {
    "LLVM_SYS_181_PREFIX", "LLVM_CONFIG_PATH", "CARGO_BUILD_JOBS", "CARGO_INCREMENTAL",
    "CARGO_TARGET_DIR", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUST_TEST_THREADS",
    "RUST_TEST_NOCAPTURE", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
    "CC", "CXX", "AR", "LD", "LIBRARY_PATH", "LD_LIBRARY_PATH", "PKG_CONFIG_PATH",
    "ASAN_OPTIONS", "LSAN_OPTIONS", "TSAN_OPTIONS", "UBSAN_OPTIONS",
}
TEST_RESULT = re.compile(
    r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
    r"(\d+) measured; (\d+) filtered out"
)


def sha(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def content_hash(value):
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    return hashlib.sha256(encoded).hexdigest()


def save(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.write("\n")


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root)


def inputs(root, extras):
    tracked = {os.fsdecode(item) for item in git(root, "ls-files", "-z").split(b"\0") if item}
    paths = set(tracked)
    for name in (*INPUT_DIRS, *ROOT_INPUTS, *extras):
        candidate = root / name
        if candidate.is_dir():
            for directory, dirs, files in os.walk(candidate):
                dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS and not d.startswith(".tmp"))
                for filename in files:
                    file = Path(directory) / filename
                    relative = str(file.relative_to(root))
                    # Keep every tracked file. Auto-discovery excludes generated
                    # native products; --input can explicitly include such inputs.
                    generated = file.suffix in OUTPUT_SUFFIXES or (
                        not file.suffix and file.stat().st_mode & 0o111 and name != "scripts")
                    if relative in tracked or not generated or name in extras:
                        paths.add(relative)
        elif candidate.exists() or candidate.is_symlink():
            paths.add(str(candidate.relative_to(root)))
        elif name in extras:
            raise RuntimeError(f"missing explicit input: {name}")
    result = {}
    for name in sorted(paths):
        path = root / name
        if path.is_symlink():
            record = {"symlink": os.readlink(path)}
            if path.is_file():
                record["sha256"] = sha(path)
        elif path.is_file():
            record = {"sha256": sha(path), "mode": path.stat().st_mode & 0o777}
        elif not path.exists():
            record = {"missing": True}
        else:
            raise RuntimeError(f"non-file tracked input requires explicit support: {name}")
        record["tracked"] = name in tracked
        result[name] = record
    return result


def snapshot(root, extras):
    files = inputs(root, extras)
    result = {
        "commit": git(root, "rev-parse", "HEAD").decode().strip(),
        "branch": git(root, "branch", "--show-current").decode().strip(),
        "tracked_diff_hash": hashlib.sha256(git(root, "diff", "HEAD", "--binary")).hexdigest(),
        "collector_sha256": sha(SCRIPT),
        "sources": files,
        "source_tree_hash": content_hash(files),
        "untracked_inputs": {name: item for name, item in files.items() if not item["tracked"]},
        "explicit_inputs": extras,
        "limitations": ["Source stability compares boundary snapshots; edits reverted during execution are not observed.",
                        "External toolchain, dependency, FFI and runtime-cache identities need their actual artifact records."],
    }
    result["source_identity"] = content_hash({key: result[key] for key in (
        "commit", "tracked_diff_hash", "collector_sha256", "source_tree_hash")})
    return result


def environment():
    return {key: value for key, value in os.environ.items()
            if key in ENV_KEYS or key.startswith(("CARGO_PROFILE_", "CARGO_TARGET_", "MIMI_"))}


def toolchain():
    """Read version commands and actual files; missing tools stay explicitly missing."""
    llvm_prefix = os.environ.get("LLVM_SYS_181_PREFIX")
    llvm = str(Path(llvm_prefix) / "bin/llvm-config") if llvm_prefix else "llvm-config-18"
    commands = {"rustc": [os.environ.get("RUSTC", "rustc"), "--version", "--verbose"],
                "cargo": ["cargo", "--version"], "llvm": [llvm, "--version"],
                "z3": ["z3", "--version"], "cc": [os.environ.get("CC", "cc"), "--version"]}
    result = {"platform": {"sysname": os.uname().sysname, "machine": os.uname().machine}, "tools": {}}
    for name, command in commands.items():
        executable = shutil.which(command[0])
        if not executable:
            result["tools"][name] = {"command": command, "status": "missing", "exit_code": None}
            continue
        process = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        result["tools"][name] = {"command": command, "status": "observed",
            "exit_code": process.returncode, "version_output": process.stdout.decode(errors="replace"),
            "executable": artifact(executable)}
    rustc = commands["rustc"][0]
    if shutil.which(rustc):
        probe = subprocess.run([rustc, "--print", "sysroot"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if probe.returncode == 0:
            actual_rustc = Path(os.fsdecode(probe.stdout).strip()) / "bin/rustc"
            if actual_rustc.is_file():
                result["tools"]["rustc"]["toolchain_executable"] = artifact(actual_rustc)
    libraries = []
    for library in (Path("/usr/lib/x86_64-linux-gnu/libz3.so"), Path("/usr/lib/x86_64-linux-gnu/libLLVM-18.so")):
        if library.is_file():
            libraries.append(artifact(library))
    result["host_library_artifacts"] = libraries
    result["limitation"] = "Version/hash probes describe observed tools; explicit --artifact-file records are required for actual external runtime/FFI/native products."
    return result


def artifact(path):
    path = Path(path).resolve()
    return {"path": str(path), "sha256": sha(path), "bytes": path.stat().st_size}


def verify_artifacts(entries):
    for entry in entries:
        path = Path(entry["path"])
        if not path.is_file() or sha(path) != entry["sha256"]:
            raise RuntimeError(f"artifact missing or changed after build: {path}")


class BoundedArchiveWriter:
    """Stop a new source archive before it consumes unbounded disk space."""

    def __init__(self, stream):
        self.stream = stream

    def write(self, data):
        if self.stream.tell() + len(data) > MAX_SOURCE_ARCHIVE_BYTES:
            raise RuntimeError("source archive exceeds the 48 MiB storage budget")
        return self.stream.write(data)

    def flush(self):
        return self.stream.flush()

    def tell(self):
        return self.stream.tell()


def archive_sources(args, baseline):
    """Save the exact snapshot bytes, rejecting drift and preserving failures."""
    destination = args.evidence / "source-bundle.tar.gz"
    manifest_path = args.evidence / "source-bundle.json"
    failure_path = args.evidence / "source-bundle-failure.json"
    if any(path.exists() for path in (destination, manifest_path, failure_path)):
        raise RuntimeError("preserve existing source bundle or archive failure")
    source_bytes = 0
    try:
        source_bytes = sum((args.root / name).stat().st_size for name, item in baseline["sources"].items()
                           if "sha256" in item)
        if source_bytes > MAX_SOURCE_INPUT_BYTES:
            raise RuntimeError(f"source inputs exceed 64 MiB budget: {source_bytes} bytes")
        available = shutil.disk_usage(args.evidence).free
        if available < source_bytes + 8 * 1024 * 1024:
            raise RuntimeError(f"insufficient source archive space: available={available}, inputs={source_bytes}")
        members, symlinks, missing = [], [], []

        def add_bytes(archive, name, data, mode=0o644):
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.mtime = len(data), mode, 0
            archive.addfile(info, io.BytesIO(data))
            members.append({"member": name, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)})

        with destination.open("xb") as output:
            with tarfile.open(fileobj=BoundedArchiveWriter(output), mode="w:gz") as archive:
                for name, expected in baseline["sources"].items():
                    path = args.root / name
                    if expected.get("missing"):
                        if path.exists() or path.is_symlink():
                            raise RuntimeError(f"deleted input reappeared during archive: {name}")
                        missing.append(name)
                        continue
                    if "symlink" in expected:
                        if not path.is_symlink() or os.readlink(path) != expected["symlink"]:
                            raise RuntimeError(f"symlink changed during source archive: {name}")
                        resolved = path.resolve()
                        external = not resolved.is_relative_to(args.root)
                        info = tarfile.TarInfo("sources/" + name)
                        info.type, info.linkname, info.mode = tarfile.SYMTYPE, expected["symlink"], 0o777
                        archive.addfile(info)
                        symlink = {"input": name, "target": expected["symlink"],
                                   "resolved_target": str(resolved), "external_target": external}
                        symlinks.append(symlink)
                        if "sha256" not in expected:
                            symlink["target_content"] = "not hashed by source snapshot; directory or dangling target is not archived"
                            continue
                        member_name = "symlink-target-contents/" + name
                        symlink["content_member"] = member_name
                    else:
                        if path.is_symlink() or not path.is_file():
                            raise RuntimeError(f"regular input type changed during archive: {name}")
                        if path.stat().st_mode & 0o777 != expected["mode"]:
                            raise RuntimeError(f"input mode changed during source archive: {name}")
                        member_name = "sources/" + name
                    data = path.read_bytes()
                    if hashlib.sha256(data).hexdigest() != expected["sha256"]:
                        raise RuntimeError(f"input content changed during source archive: {name}")
                    add_bytes(archive, member_name, data, expected.get("mode", 0o644))
                inner = {"source_identity": baseline["source_identity"],
                         "sources": baseline["sources"], "members": members,
                         "symlinks": symlinks, "missing_inputs": missing}
                add_bytes(archive, "bundle-manifest.json", json.dumps(inner, sort_keys=True, ensure_ascii=False).encode())
        current = snapshot(args.root, args.input)
        if current["source_identity"] != baseline["source_identity"]:
            raise RuntimeError("source changed between snapshot and completed source archive")
        manifest = {"schema_version": 1, "accepted": True,
                    "source_identity": baseline["source_identity"], "collector_sha256": sha(SCRIPT),
                    "archive": artifact(destination), "input_content_bytes": source_bytes,
                    "members": members, "symlinks": symlinks, "missing_inputs": missing,
                    "limitations": ["Symlink targets retain their original metadata; external paths are not reconstructed automatically.",
                                    "External directory/dangling symlink payloads, toolchains and runtime/FFI identities are not solved by this source bundle."]}
        save(manifest_path, manifest)
        print(f"archived {source_bytes} input bytes into {destination.stat().st_size} bytes", flush=True)
        return {**manifest, "manifest": artifact(manifest_path)}
    except (OSError, RuntimeError, tarfile.TarError) as error:
        failure = {"accepted": False, "source_identity": baseline["source_identity"],
                   "error": str(error), "input_content_bytes": source_bytes,
                   "partial_archive": artifact(destination) if destination.is_file() else None}
        save(failure_path, failure)
        raise RuntimeError(f"source archive rejected: {error}") from error


def counts(log):
    text = log.read_text(errors="replace")
    summaries = [{"status": row[0], **dict(zip(
        ("passed", "failed", "ignored", "measured", "filtered_out"), map(int, row[1:]))) }
        for row in TEST_RESULT.findall(text)]
    declared = sum(map(int, re.findall(r"^running (\d+) tests?$", text, re.MULTILINE)))
    totals = {key: sum(row[key] for row in summaries)
              for key in ("passed", "failed", "ignored", "measured", "filtered_out")}
    return {"summaries": summaries, "declared": declared,
            "executed": totals["passed"] + totals["failed"] + totals["measured"],
            **totals, "discovered": sum(totals.values())}


def execute(args, name, command, baseline, *, required_tests=False, selected=None, json_log=False):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", name):
        raise RuntimeError("result name must use letters, digits, dots, dashes or underscores")
    log = args.evidence / (name + (".jsonl" if json_log else ".log"))
    record_path = args.evidence / (name + ".json")
    if log.exists() or record_path.exists():
        raise RuntimeError(f"preserve existing evidence: {name}")
    before = snapshot(args.root, args.input)
    if before["source_identity"] != baseline["source_identity"]:
        raise RuntimeError("source differs from frozen build; create a new evidence directory")
    if selected:
        verify_artifacts([selected])
    observed_toolchain = toolchain() if "toolchain" in baseline else None
    if observed_toolchain is not None and observed_toolchain != baseline["toolchain"]:
        raise RuntimeError("toolchain identity differs from frozen build")
    started = time.time()
    print(f"running {name}: {json.dumps(command, ensure_ascii=False)}", flush=True)
    with log.open("xb") as output:
        try:
            result = subprocess.run(command, cwd=args.root, stdout=output, stderr=subprocess.STDOUT)
            exit_code = result.returncode
            launch_error = None
        except OSError as error:
            exit_code = None
            launch_error = str(error)
            output.write((launch_error + "\n").encode())
    ended = time.time()
    after = snapshot(args.root, args.input)
    stable = before["source_identity"] == after["source_identity"]
    test_counts = counts(log)
    accepted = exit_code == 0 and stable and (not required_tests or (
        test_counts["executed"] > 0 and bool(test_counts["summaries"]) and
        test_counts["failed"] == 0 and all(row["status"] == "ok" for row in test_counts["summaries"])))
    extra_artifacts = []
    for filename in args.artifact_file:
        path = Path(filename)
        if path.is_file():
            extra_artifacts.append(artifact(path))
        else:
            extra_artifacts.append({"path": str(path.resolve()), "sha256": None, "status": "missing"})
            accepted = False
    selected_stable = selected is None or (Path(selected["path"]).is_file() and sha(selected["path"]) == selected["sha256"])
    accepted = accepted and selected_stable
    record = {
        "schema_version": 1, "evidence_id": name, "command": command,
        "cwd": str(args.root), "environment": environment(), "exit_code": exit_code,
        "launch_error": launch_error, "started_at_unix": started, "ended_at_unix": ended,
        "elapsed_seconds": ended - started, "collector_sha256": sha(SCRIPT),
        "source_identity": baseline["source_identity"], "source_stable": stable,
        "source_after_identity": after["source_identity"],
        "toolchain": observed_toolchain, "selected_artifact_stable": selected_stable,
        "changed_inputs": [path for path in before["sources"].keys() | after["sources"].keys()
                           if before["sources"].get(path) != after["sources"].get(path)],
        "selected_artifact": selected, "extra_artifacts": extra_artifacts,
        "test_counts": test_counts, "requires_nonzero_executed_tests": required_tests,
        "accepted": accepted,
        "result_class": "已通过" if accepted else ("环境未验证" if launch_error else "未解释失败"),
        "raw_log_path": str(log), "raw_log_sha256": sha(log),
        "limitation": "Exit/test counts and identity checks do not prove semantic or L3 oracles.",
    }
    save(record_path, record)
    print(json.dumps({"evidence_id": name, "exit_code": exit_code, "accepted": accepted,
                      "source_stable": stable, "test_counts": test_counts}, ensure_ascii=False), flush=True)
    return record


def build(args):
    baseline = snapshot(args.root, args.input)
    baseline["toolchain"] = toolchain()
    save(args.evidence / "source-before-build.json", baseline)
    source_bundle = archive_sources(args, baseline)
    record = execute(args, "build", args.command, baseline, json_log=True)
    record["source_bundle"] = source_bundle
    if not record["accepted"]:
        (args.evidence / "build.json").write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")
        return 2
    entries, files, finished = [], {}, []
    for line in Path(record["raw_log_path"]).read_text(errors="replace").splitlines():
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if entry.get("reason") == "build-finished":
            finished.append(entry.get("success") is True)
        if entry.get("reason") != "compiler-artifact":
            continue
        for filename in entry.get("filenames", []):
            path = Path(filename)
            if path.is_file():
                files[str(path.resolve())] = artifact(path)
        if entry.get("executable"):
            entries.append({**artifact(entry["executable"]), "target": entry["target"]["name"],
                            "kind": entry["target"]["kind"], "test": entry["profile"]["test"],
                            "fresh": entry.get("fresh"), "profile": entry["profile"],
                            "features": entry.get("features", []), "package_id": entry["package_id"]})
    record["cargo_build_finished_success"] = bool(finished) and all(finished)
    record["executable_artifact_count"] = len(entries)
    record["cargo_output_artifact_count"] = len(files)
    record["accepted"] = record["accepted"] and bool(entries) and record["cargo_build_finished_success"]
    record["result_class"] = "已通过" if record["accepted"] else "未解释失败"
    # Replace only our just-written provisional record; never overwrite a prior run.
    record_path = args.evidence / "build.json"
    record_path.write_text(json.dumps(record, indent=2, ensure_ascii=False) + "\n")
    manifest = {"source_identity": baseline["source_identity"], "build_record": record,
                "executable_artifacts": entries, "cargo_output_artifacts": list(files.values())}
    save(args.evidence / "artifacts.json", manifest)
    if not record["accepted"]:
        print("Build requires executable artifacts and Cargo build-finished success; identity rejected.", flush=True)
        return 2
    print(f"recorded {len(entries)} executable artifacts", flush=True)
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=SCRIPT.parent.parent)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--input", action="append", default=[], help="additional repository-relative input file/directory")
    parser.add_argument("--artifact-file", action="append", default=[], help="actual extra native/runtime/FFI artifact to hash")
    sub = parser.add_subparsers(dest="mode", required=True)
    sub.add_parser("snapshot")
    build_parser = sub.add_parser("build")
    build_parser.add_argument("command", nargs=argparse.REMAINDER)
    run = sub.add_parser("run")
    run.add_argument("--name", required=True)
    run.add_argument("--target", required=True)
    run.add_argument("--kind", required=True)
    run.add_argument("command", nargs=argparse.REMAINDER)
    all_tests = sub.add_parser("run-all")
    all_tests.add_argument("--test-threads", type=int, default=4)
    command = sub.add_parser("command")
    command.add_argument("--name", required=True)
    command.add_argument("--require-tests", action="store_true")
    command.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    args.root = args.root.resolve()
    args.evidence = args.evidence.resolve()
    args.evidence.mkdir(parents=True, exist_ok=True)
    if hasattr(args, "command") and args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if args.mode in ("build", "command") and not args.command:
        parser.error("a command after -- is required")
    if args.mode == "snapshot":
        save(args.evidence / "source-snapshot.json", snapshot(args.root, args.input))
        return 0
    if args.mode == "build":
        return build(args)
    baseline = json.loads((args.evidence / "source-before-build.json").read_text())
    manifest = json.loads((args.evidence / "artifacts.json").read_text())
    if not manifest["build_record"]["accepted"] or manifest["source_identity"] != baseline["source_identity"]:
        raise RuntimeError("unaccepted or mismatched build manifest")
    if args.input != baseline["explicit_inputs"]:
        raise RuntimeError("explicit input list differs from build")
    verify_artifacts(manifest["build_record"]["extra_artifacts"])
    candidates = [entry for entry in manifest["executable_artifacts"] if entry["test"]]
    if args.mode == "run":
        candidates = [entry for entry in candidates if entry["target"] == args.target and args.kind in entry["kind"]]
        if len(candidates) != 1:
            raise RuntimeError(f"expected one test artifact; found {len(candidates)}")
        selected = candidates[0]
        result = execute(args, args.name, [selected["path"], *args.command], baseline,
                         required_tests=True, selected=selected)
        return 0 if result["accepted"] else 2
    if args.mode == "run-all":
        candidates = [entry for entry in candidates if "bench" not in entry["kind"]]
        if not candidates:
            raise RuntimeError("no test artifacts")
        results = []
        for index, entry in enumerate(candidates):
            # A bin may contain zero tests; lib/integration suites must execute tests.
            result = execute(args, f"all-{index:02d}-{entry['target']}",
                             [entry["path"], f"--test-threads={args.test_threads}"], baseline,
                             required_tests="bin" not in entry["kind"], selected=entry)
            results.append(result)
        save(args.evidence / "run-all.json", {"artifacts": len(results),
             "accepted": all(row["accepted"] for row in results),
             "executed_tests": sum(row["test_counts"]["executed"] for row in results),
             "ignored_tests": sum(row["test_counts"]["ignored"] for row in results)})
        return 0 if all(row["accepted"] for row in results) else 2
    verify_artifacts(manifest["executable_artifacts"])
    result = execute(args, args.name, args.command, baseline, required_tests=args.require_tests)
    return 0 if result["accepted"] else 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        print(f"evidence rejected: {error}", file=sys.stderr)
        sys.exit(2)
