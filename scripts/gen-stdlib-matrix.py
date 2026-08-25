#!/usr/bin/env python3
"""Stdlib 全模块 × 全组合 矩阵 probe 生成器（MCDD sweep 0.39.x）。

对 std/*.mimi 的每一个 `pub func` 生成最小调用样例：
  singles/<mod>.mimi        该模块全部 pub 函数各调用一次
  pairs/<a>__<b>.mimi       两模块代表性调用共存（loader 合并 / 名字冲突面）
  mega.mimi                 全部模块共存
  traps/<mod>_<fn>.mimi     按设计即陷阱的函数（fail 等）单独验证退出码对拍

用法: python3 scripts/gen-stdlib-matrix.py [--out tests/stdlib_matrix_generated]
"""
from __future__ import annotations

import argparse
import itertools
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
STD = REPO / "std"

SIG_RE = re.compile(
    r"^pub func ([A-Za-z_][A-Za-z0-9_]*)(<[^()>]*>)?\((.*)\)\s*(?:->\s*(.+?))?\s*\{$"
)


def split_params(s: str) -> list[tuple[str, str]]:
    """Top-level comma split respecting (), [], <> nesting（先中和 -> 箭头）."""
    s = s.replace("->", "\x01")
    params, depth, cur = [], 0, ""
    for ch in s:
        if ch in "(<[":
            depth += 1
        elif ch in ")>]":
            depth -= 1
        if ch == "," and depth == 0:
            params.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        params.append(cur.strip())
    out = []
    for p in params:
        p = p.replace("\x01", "->")
        if ":" not in p:
            continue  # self/other malformed
        name, ty = p.split(":", 1)
        out.append((name.strip(), ty.strip()))
    return out


def parse_sig(line: str):
    m = re.match(r"^pub func ([A-Za-z_][A-Za-z0-9_]*)(<[^()>]*>)?\(", line.strip())
    if not m:
        return None
    name = m.group(1)
    i = m.end()  # 恰好在参数表 '(' 之后
    depth, j = 1, i
    while j < len(line) and depth > 0:
        c = line[j]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        j += 1
    params_src = line[i : j - 1]
    rest = line[j:].strip()
    rm = re.match(r"^->\s*(.+?)\s*\{$", rest)
    ret = rm.group(1).strip() if rm else None
    return name, params_src, ret


def load_modules() -> dict[str, list[dict]]:
    mods: dict[str, list[dict]] = {}
    for f in sorted(STD.glob("*.mimi")):
        funcs = []
        for i, line in enumerate(f.read_text().splitlines(), 1):
            sig = parse_sig(line)
            if sig:
                name, params_src, ret = sig
                funcs.append(
                    {
                        "name": name,
                        "params": split_params(params_src),
                        "ret": ret or None,
                        "line": i,
                    }
                )
        mods[f.stem] = funcs
    return mods


# --------------------------------------------------------------------------
# 每函数覆盖表：args / stmts（完整替换语句）/ uses（额外 use）/ skip / helpers
# --------------------------------------------------------------------------

OVERRIDES: dict[str, dict] = {
    # ---- array ----
    "array.array_get": {"args": '["b", "a", "c"], 1'},
    "array.array_set": {"args": '["b", "a"], 0, "z"'},
    "array.array_slice": {"args": '["b", "a", "c"], 0, 2'},
    "array.array_binary_search": {"args": '["a", "b", "d"], "b"'},
    "array.array_rotate_left": {"args": '["b", "a", "c"], 1'},
    "array.array_rotate_right": {"args": '["b", "a", "c"], 1'},
    "array.array_take": {"args": '["b", "a", "c"], 2'},
    "array.array_drop": {"args": '["b", "a", "c"], 1'},
    # ---- collections（泛型钉类型）----
    "collections.fill_list": {"args": '2, "x"'},
    "collections.map_list": {"args": '[1, 2], fn(x: i32) -> i32 { x * 10 }'},
    "collections.any": {"args": '[1, 2], fn(x: i32) -> bool { x > 1 }'},
    "collections.all": {"args": '[1, 2], fn(x: i32) -> bool { x > 0 }'},
    "collections.find_map": {
        "ret_shape": "option",
        "args": '[1, 2, 3], fn(x: i32) -> (bool, i32) { (x > 1, x * 7) }',
    },
    "collections.partition": {
        "ret_shape": "tuple",
        "args": '[1, 2, 3, 4], fn(x: i32) -> bool { x % 2 == 0 }',
    },
    "collections.filter_list": {"args": '[1, 2, 3], fn(x: i32) -> bool { x > 1 }'},
    "collections.reduce_list": {"args": '[1, 2, 3], fn(a: i32, b: i32) -> i32 { a + b }, 10'},
    "collections.remove_at": {"args": "[3, 1, 2], 1"},
    "collections.remove_value": {"args": "[3, 1, 2], 1"},
    # ---- datetime（时钟面 → 确定性谓词化）----
    "datetime.now_secs": {"stmts": ['println(now_secs() > 1000000000);']},
    "datetime.now_millis": {"stmts": ['println(now_millis() > 1000000000000);']},
    "datetime.timestamp_secs": {"stmts": ['println(timestamp_secs() > 1000000000);']},
    "datetime.timestamp_millis": {"stmts": ['println(timestamp_millis() > 1000000000000);']},
    "datetime.days_from_now": {"stmts": ['println(days_from_now(3) > now_secs());']},
    "datetime.hours_from_now": {"stmts": ['println(hours_from_now(3) > now_secs());']},
    "datetime.minutes_from_now": {"stmts": ['println(minutes_from_now(3) > now_secs());']},
    "datetime.is_future": {"stmts": ['println(is_future(now_secs() + 10000));']},
    "datetime.is_past": {"stmts": ['println(is_past(now_secs() - 10000));']},
    "datetime.time_since": {"stmts": ['println(time_since(now_secs() - 10000) >= 10000);']},
    "datetime.time_until": {"stmts": ['println(time_until(now_secs() + 10000) <= 10000);']},
    "datetime.elapsed_since": {"stmts": ['println(elapsed_since(now_millis()) >= 0);']},
    "datetime.sleep_secs": {"stmts": ["sleep_secs(0);"]},
    "datetime.sleep_until": {"stmts": ["sleep_until(now_secs() - 100);"]},
    # ---- env（argv 面 = 刻意的 L1 试金石）----
    "env.get_var": {"ret_shape": "result", "args": '"__MISSING_MM_PROBE__"'},
    "env.get_var_or": {"args": '"__MISSING_MM_PROBE__, ", "fallback"'},
    "env.get_int": {"ret_shape": "result", "args": '"__MISSING_MM_PROBE__"'},
    "env.get_float": {"ret_shape": "result", "args": '"__MISSING_MM_PROBE__"'},
    "env.cli_args": {"stmts": [
        "let ea = cli_args();",
        "println(len(ea));",
    ]},
    "env.arg_count": {"stmts": ["println(arg_count());"]},
    "env.first_arg": {"stmts": ["println(first_arg());"]},
    "env.set_var": {"stmts": [
        'println(set_var("MM_PROBE_VAR", "42"));',
        'match get_var("MM_PROBE_VAR") { Ok(v) => { println(v) } Err(e) => { println(e) } }',
        'match get_int("MM_PROBE_VAR") { Ok(v) => { println(v) } Err(e) => { println(e) } }',
    ]},
    # ---- errors（变体构造 + 显示）----
    "errors.fs_error_to_string": {"args": 'FsError.NotFound("x")'},
    "errors.json_error_to_string": {"args": 'JsonError.MissingField("k")'},
    "errors.collection_error_to_string": {"args": 'CollectionError.EmptyCollection'},
    "errors.net_error_to_string": {"args": 'NetError.Timeout("t")'},
    "errors.math_error_to_string": {"args": 'MathError.DivisionByZero'},
    "errors.app_error_to_string": {"args": 'AppError.Custom("c")'},
    # ---- fs（scratch 文件往返；runner 负责双后端间清理）----
    "fs.exists": {"args": '"mm_probe_a.txt"'},
    "fs.write": {"ret_shape": "result", "args": '"mm_probe_a.txt", "hello"'},
    "fs.read": {"ret_shape": "result", "args": '"mm_probe_a.txt"'},
    "fs.read_lines": {"ret_shape": "result", "args": '"mm_probe_a.txt"'},
    "fs.file_size": {"ret_shape": "result", "args": '"mm_probe_a.txt"'},
    "fs.write_lines": {"ret_shape": "result", "args": '"mm_probe_b.txt", ["x", "y"]'},
    "fs.stat": {"stmts": [
        'write_file("mm_probe_c.txt", "abcd");',
        "let st = stat(\"mm_probe_c.txt\");",
        "println(st.size);",
        "println(st.is_file);",
        "println(st.is_dir);",
    ]},
    "fs.append": {"stmts": [
        'println(append("mm_probe_d.txt", "a"));',
        'println(append("mm_probe_d.txt", "b"));',
        'match read("mm_probe_d.txt") { Ok(v) => { println(v) } Err(e) => { println(e) } }',
    ]},
    "fs.read_partial": {"args": '"mm_probe_a.txt", 4'},
    "fs.read_bytes": {"args": '"mm_probe_a.txt"'},
    "fs.write_bytes": {"ret_shape": "print", "args": '"mm_probe_e.txt", "xyz"'},
    "fs.read_lines_json": {"args": '"mm_probe_b.txt"'},
    "fs.read": {},
    # ---- io ----
    "io.print_line": {"args": '"pl"'},
    "io.print_raw": {"args": '"pr"'},
    "io.print_format": {"args": '["pf", "!"]'},
    "io.print_err": {"stmts": ['print_err("stderr-line");']},
    "io.print_lines": {"args": '["x", "y"]'},
    "io.print_bool": {"args": "true"},
    "io.print_int": {"args": "31"},
    "io.print_float": {"args": "2.5"},
    "io.print_list": {"args": "[1, 2, 3]"},
    "io.input_line": {"ret_shape": "result"},   # stdin=/dev/null → EOF 错误路径
    "io.input_int": {"ret_shape": "result"},
    "io.input_float": {"ret_shape": "result"},
    "io.input_bool": {"ret_shape": "result"},
    # ---- json ----
    "json.get_string": {"args": '"{\\"a\\": \\"b\\"}", "a"'},
    "json.get_element": {"args": '"[10, 20]", 1'},
    "json.get_bool": {"ret_shape": "result", "args": '"{\\"f\\": true}", "f"'},
    "json.get_object": {"args": '"{\\"o\\": {\\"k\\": 1}}", "o"'},
    "json.get_array": {"args": '"{\\"a\\": [1, 2]}", "a"'},
    "json.to_string_pretty": {"args": '"{\\"a\\": 1}"'},
    "json.array_length": {"args": '"[1, 2, 3]"'},
    # ---- maps ----
    "maps.new": {"ret_shape": "record"},
    "maps.get": {"ret_shape": "tuple", "args": "map_from_list([]), \"k\""},
    "maps.set": {"ret_shape": "record", "args": "map_from_list([]), \"k\", 7"},
    "maps.has_key": {"args": "map_from_list([]), \"k\""},
    "maps.remove": {"ret_shape": "record", "args": "map_from_list([]), \"k\""},
    "maps.size": {"args": "map_from_list([])"},
    "maps.from_list": {"ret_shape": "record", "stmts": [
        "let mfl_b = new();",
        'let mfl_c = set(set(mfl_b, "a", 1), "b", 2);',
        "println(size(mfl_c));",
        "let mfl_rt = from_list(to_list(mfl_c));",
        "println(size(mfl_rt));",
    ]},
    "maps.get_or_default": {"args": "map_from_list([]), \"k\", 99"},
    "maps.merge": {"ret_shape": "record",
                   "args": "map_from_list([]), map_from_list([])"},
    "maps.to_list": {"stmts": [
        "let tl = to_list(map_from_list([]));",
        "println(len(tl));",
    ]},
    "maps.filter_keys": {"ret_shape": "record",
                         "args": 'map_from_list([]), fn(k: string) -> bool { k == "a" }'},
    "maps.map_values": {"ret_shape": "record",
                        "args": "map_from_list([]), fn(v: Any) -> Any { v }"},
    "maps.update": {"ret_shape": "record",
                    "args": 'map_from_list([]), "k", fn(v: Any) -> Any { v }'},
    "maps.pick": {"ret_shape": "record", "args": 'map_from_list([]), ["a"]'},
    "maps.omit": {"ret_shape": "record", "args": 'map_from_list([]), ["a"]'},
    # ---- mymath 域敏感样例 ----
    "mymath.factorial": {"args": "6"},
    "mymath.fibonacci": {"args": "10"},
    "mymath.is_prime": {"args": "17"},
    "mymath.mod_pow": {"args": "7, 3, 5"},
    "mymath.deg_to_rad": {"args": "180.0"},
    "mymath.rad_to_deg": {"args": "3.14159265"},
    "mymath.map_range": {"args": "5.0, 0.0, 10.0, -1.0, 1.0"},
    "mymath.next_power_of_two": {"args": "33"},
    "mymath.count_digits": {"args": "12345"},
    "mymath.digit_at": {"args": "12345, 2"},
    "mymath.sum_digits": {"args": "1234"},
    "mymath.reverse_number": {"args": "1234"},
    "mymath.is_palindrome_number": {"args": "121"},
    "mymath.collatz_steps": {"args": "6"},
    "mymath.power": {"args": "2.0, 3.0"},
    "mymath.sqrt_val": {"args": "9.0"},
    "mymath.floor_val": {"args": "2.7"},
    "mymath.ceil_val": {"args": "2.1"},
    "mymath.round_val": {"args": "2.5"},
    "mymath.gcd": {"args": "48, 18"},
    "mymath.lcm": {"args": "4, 6"},
    "mymath.hypot": {"args": "3.0, 4.0"},
    "mymath.my_asin": {"args": "0.5"},
    "mymath.my_acos": {"args": "0.5"},
    "mymath.my_atan2": {"args": "1.0, 2.0"},
    "mymath.my_ln": {"args": "1.0"},
    "mymath.my_log": {"args": "8.0, 2.0"},
    "mymath.my_log2": {"args": "8.0"},
    "mymath.my_log10": {"args": "100.0"},
    "mymath.my_exp": {"args": "1.0"},
    "mymath.try_div": {"ret_shape": "result", "args": "7, 2"},
    "mymath.try_mod": {"ret_shape": "result", "args": "7, 2"},
    "mymath.try_factorial": {"ret_shape": "result", "args": "5"},
    "mymath.try_sqrt": {"ret_shape": "result", "args": "16.0"},
    "mymath.try_ln": {"ret_shape": "result", "args": "1.0"},
    "mymath.try_log": {"ret_shape": "result", "args": "8.0, 2.0"},
    "mymath.try_pow_int": {"ret_shape": "result", "args": "2, 10"},
    # 随机面 → 确定性谓词
    "mymath.random_normal": {"stmts": [
        "let rv = random_normal();",
        "println(rv > -1000000.0 && rv < 1000000.0);",
    ]},
    "mymath.random_uniform": {"stmts": [
        "let ru = random_uniform(2.0, 5.0);",
        "println(ru >= 2.0 && ru < 5.0);",
    ]},
    "mymath.random_exponential": {"stmts": [
        "let re = random_exponential(1.0);",
        "println(re >= 0.0);",
    ]},
    "mymath.random_bernoulli": {"stmts": [
        "let rb = random_bernoulli(0.5);",
        "if rb || !rb { println(1) } else { println(0) }",
    ]},
    "mymath.random_int_range": {"stmts": [
        "let ri = random_int_range(5, 9);",
        "println(ri >= 5);",
    ]},
    # ---- net（错误路径为主；本机回环即时拒绝）----
    "net.tcp_socket": {"stmts": ["println(tcp_socket() >= 0);"]},
    "net.tcp_connect": {"ret_shape": "result", "args": '"127.0.0.1", 9'},
    "net.tcp_listen": {"ret_shape": "result", "args": "0, 1"},
    "net.tcp_accept": {"ret_shape": "result", "args": "-1"},
    "net.tcp_send": {"ret_shape": "result", "args": '-1, "x"'},
    "net.tcp_recv": {"ret_shape": "result", "args": "-1, 64"},
    "net.fetch": {"ret_shape": "result", "args": '"http://127.0.0.1:9/"'},
    "net.fetch_post": {"ret_shape": "result", "args": '"http://127.0.0.1:9/", "{}"'},
    # ---- prelude ----
    "prelude.identity": {"args": "5"},
    "prelude.const_val": {"args": '1, "u"'},
    "prelude.negate": {"args": "true"},
    "prelude.swap": {"ret_shape": "tuple", "args": '1, "a"'},
    "prelude.compose": {"stmts": [
        "let h = compose(fn(y: i32) -> i32 { y + 1 }, fn(z: i32) -> i32 { z * 2 });",
        "println(h(3));",
    ]},
    "prelude.pipe": {"args": "3, fn(x: i32) -> i32 { x + 1 }"},
    "prelude.tap": {"args": "5, fn(x: i32) -> () { }"},
    "prelude.flip": {"stmts": [
        "let fh = flip(fn(a: i32, b: i32) -> i32 { a * 10 + b });",
        "println(fh(1, 2));",
    ]},
    "prelude.apply": {"args": "fn(x: i32) -> i32 { x + 1 }, 4"},
    "prelude.konst": {"stmts": [
        "let kc = konst::<i32, string>(9);",
        'println(kc("ignored"));',
    ]},
    "prelude.eq": {"args": "1, 1"},
    "prelude.not_eq": {"args": "1, 2"},
    "prelude.repeat_action": {"args": "3, fn(i: i32) -> () { }"},
    "prelude.times": {"args": "2, fn() -> () { }"},
    "prelude.to_int_safe": {"args": '"41", 0'},
    "prelude.to_float_safe": {"args": '"2.5", 0.0'},
    "prelude.half": {"args": "7"},
    # 设计上即陷阱的三个 → 单独 trap probe 验证退出码对拍
    "prelude.fail": {"skip": "trap-by-design; covered by traps/prelude_fail.mimi"},
    "prelude.unreachable": {"skip": "trap-by-design; covered by traps/prelude_unreachable.mimi"},
    "prelude.todo": {"skip": "trap-by-design; covered by traps/prelude_todo.mimi"},
    # ---- random ----
    "random.random_float": {"stmts": [
        "let rf = random_float(2.0, 5.0);",
        "println(rf >= 2.0 && rf < 5.0);",
    ]},
    "random.random_int": {"stmts": [
        "let rn = random_int(5, 9);",
        "println(rn >= 5);",
    ]},
    "random.random_bool": {"stmts": [
        "let rb2 = random_bool();",
        "if rb2 || !rb2 { println(1) } else { println(0) }",
    ]},
    "random.random_choice": {"ret_shape": "result", "args": "[42]"},
    "random.random_sample": {"stmts": [
        "let rs = random_sample([1, 2, 3], 0);",
        "println(len(rs));",
    ]},
    "random.shuffle": {"stmts": [
        "let sh = shuffle([3, 1, 2]);",
        "println(sort_list(sh));",
    ], "uses_extra": ["collections"]},
    "random.random_remove_ith": {"args": "[7, 8, 9], 0"},
    # ---- result ----
    "result.is_ok_result": {"stmts": [
        "let rok: Result<i32, string> = Ok(7);",
        "println(is_ok_result(rok));",
    ]},
    "result.is_err_result": {"stmts": [
        "let rerr: Result<i32, string> = Err(\"bad\");",
        "println(is_err_result(rerr));",
    ]},
    "result.result_unwrap": {"stmts": [
        "let rw: Result<i32, string> = Ok(7);",
        "println(result_unwrap(rw));",
    ]},
    "result.unwrap_or": {"stmts": [
        "let ro: Result<i32, string> = Err(\"e\");",
        "println(unwrap_or(ro, 5));",
    ]},
    "result.expect_result": {"stmts": [
        "let rx: Result<i32, string> = Ok(7);",
        "println(expect_result(rx, \"msg\"));",
    ]},
    "result.map_result": {"stmts": [
        "let rm: Result<i32, string> = Ok(7);",
        "match map_result(rm, fn(v: i32) -> i32 { v * 2 }) { Ok(v) => { println(v) } Err(e) => { println(e) } }",
    ]},
    "result.map_err_result": {"stmts": [
        "let rme: Result<i32, string> = Err(\"abc\");",
        "match map_err_result(rme, fn(e: string) -> i32 { len(e) }) { Ok(v) => { println(v) } Err(e) => { println(e) } }",
    ]},
    # ---- set（裸 Set 签名边界试探）----
    "set.insert": {"ret_shape": "set", "args": "{1, 2}, 3"},
    "set.set_size": {"args": "{1, 2}"},
    "set.set_is_empty": {"args": "{1, 2}"},
    "set.set_contains": {"args": "{1, 2}, 2"},
    "set.set_insert": {"ret_shape": "set", "args": "{1, 2}, 3"},
    "set.set_remove": {"ret_shape": "set", "args": "{1, 2}, 1"},
    "set.set_to_list": {"stmts": [
        "let sl = set_to_list({1, 2});",
        "println(len(sl));",
    ]},
    # ---- strings 抽样修正 ----
    "strings.char_at": {"args": '"mimi", 1'},
    "strings.substring": {"args": '"mimi", 0, 2'},
    "strings.index_of": {"ret_shape": "option", "args": '"mimi", "i"'},
    "strings.parse_int": {"ret_shape": "tuple", "args": '"42"'},
    "strings.parse_float": {"ret_shape": "tuple", "args": '"2.5"'},
    "strings.pad_left": {"args": '"7", 3, "0"'},
    "strings.pad_right": {"args": '"7", 3, "0"'},
    "strings.truncate": {"args": '"abcdef", 3'},
    "strings.ellipsis": {"args": '"abcdef", 4'},
    "strings.indent": {"args": '"a", 2'},
    "strings.count_char": {"args": '"banana", "a"'},
    # ---- template ----
    "template.simple_render": {
        "args": '"hi {{name}}", map_from_list([("name", "mimi")])',
    },
    "template.render": {
        "args": '"v={{k}}", fn(k: string) -> string { "V" }',
    },
    "template.lookup_with_default": {
        "args": 'map_from_list([("a", "1")]), "b", "dft"',
    },
    "template.render_with_defaults": {
        "args": '"{{x}}-{{y}}", map_from_list([("x", "1")]), "?"',
    },
    "template.render_csv": {
        "args": '"{{0}},{{1}}", [["a", "b"], ["c", "d"]]',
    },
    # ---- testing ----
    "testing.assert_eq_int": {"args": "1, 1"},
    "testing.assert_ne_int": {"args": "1, 2"},
    "testing.assert_approx_eq_float": {"args": "1.0, 1.0"},
    "testing.assert_true": {"args": "true"},
    "testing.assert_false": {"args": "false"},
    "testing.assert_eq_string": {"args": '"a", "a"'},
    "testing.assert_eq_bool": {"args": "true, true"},
    # ---- text ----
    "text.text_is_blank": {"args": '"  "', "uses_extra": []},
    "text.is_numeric": {"args": '"42"'},
    "text.text_count_lines": {"args": '"a\nb"'},
    "text.slugify": {"args": '"Hello World"'},
    "text.indent_text": {"args": '"a\nb", 2'},
    "text.wrap_text": {"args": '"aa bb cc", 5'},
    # ---- time ----
    "time.timestamp": {"stmts": ["println(timestamp() > 1000000000);"]},
    "time.timestamp_ms": {"stmts": ["println(timestamp_ms() > 1000000000000);"]},
    "time.sleep_ms": {"stmts": ["sleep_ms(1);"]},
    "time.elapsed": {"stmts": [
        "let t0 = timestamp_ms();",
        "println(elapsed(t0) >= 0);",
    ]},
    "time.seconds_since": {"stmts": [
        "let s0 = timestamp();",
        "println(seconds_since(s0) >= 0);",
    ]},
    "time.millis_since": {"stmts": [
        "let m0 = timestamp_ms();",
        "println(millis_since(m0) >= 0);",
    ]},
    "time.duration": {"args": "100, 350"},
}

TRAP_FUNCS = {
    ("prelude", "fail"): 'fail("trap-probe")',
    ("prelude", "unreachable"): "unreachable()",
    ("prelude", "todo"): "todo()",
}

# 两两组合的代表性调用（纯、确定、廉价）
PAIR_REP = {
    "array": 'println(array_len(["b", "a"]));',
    "collections": "println(sum([1, 2, 3]));",
    "crypto": 'println(hex_encode("AB"));',
    "csv": 'println(cell([["a", "b"]], 0, 1));',
    "datetime": 'println(format_duration_secs(3661));',
    "env": 'println(has_var("__MISSING_MM_PROBE__"));',
    "errors": 'println(fs_error_to_string(FsError.NotFound("x")));',
    "effects": 'println(text_is_blank(" "));',   # effects 无导出，借 text 代表性调用
    "fs": 'println(exists("mm_probe_missing_zz.txt"));',
    "io": "print_bool(true);",
    "iter": 'println(iter_repeat("x", 2));',
    "json": 'println(is_valid_json("{}"));',
    "maps": "println(size(map_from_list([])));",
    "mymath": "println(gcd(48, 18));",
    "net": "println(tcp_socket() >= 0);",
    "prelude": "println(identity(5));",
    "random": "let shp = shuffle([3, 1, 2]); println(sort_list(shp));",
    "result": "let rp: Result<i32, string> = Ok(1); println(is_ok_result(rp));",
    "set": "let sp = {1, 2}; println(set_size(sp));",
    "strings": 'println(to_upper("ab"));',
    "template": 'println(lookup_with_default(map_from_list([("a", "1")]), "b", "d"));',
    "testing": "assert_eq_int(1, 1); println(1);",
    "text": 'println(slugify("Hello World"));',
    "time": "println(duration(100, 350));",
}

PAIR_USES_EXTRA = {
    "random": ["collections"],
    "effects": ["text"],
    "maps": [],
}


PRINTABLE_SCALARS = {"i32", "i64", "f64", "bool", "string"}

_LET_SEQ = 0

DEFAULT_ARGS = {
    "i32": "7",
    "i64": "42",
    "f64": "1.5",
    "bool": "true",
    "string": '"mimi"',
    "Any": "7",
    "Record": "map_from_list([])",
    "List<i32>": "[3, 1, 2]",
    "List<f64>": "[2.5, 1.5]",
    "List<string>": '["b", "a", "c"]',
    "List<(string, Any)>": '[("a", 1), ("b", 2)]',
    "List<List<string>>": '[["a", "b"], ["c"]]',
    "List": "[1, 2]",
    "Set": "{1, 2}",
    # 泛型槽位默认钉到 i32 世界，保证确定性
    "List<T>": "[3, 1, 2]",
    "List<U>": "[3, 1, 2]",
    "List<List<T>>": "[[1, 2], [3]]",
    "T": "5",
    "U": "5",
}


def auto_args(params: list[tuple[str, str]]) -> str | None:
    parts = []
    for _name, ty in params:
        if ty not in DEFAULT_ARGS:
            return None
        parts.append(DEFAULT_ARGS[ty])
    return ", ".join(parts)


def emit_call(func: dict, ov: dict) -> tuple[list[str], list[str]]:
    """返回 (use_extra_modules, statements)。let 名带调用点序号防碰撞。"""
    global _LET_SEQ
    _LET_SEQ += 1
    tag = f"{func['name']}_{_LET_SEQ}"
    name = func["name"]
    if ov.get("stmts"):
        stmts = []
        for s in ov["stmts"]:
            stmts.append(s.replace("let tv =", f"let tv_{tag} =")
                          .replace("let mr =", f"let mr_{tag} =")
                          .replace("let ms =", f"let ms_{tag} =")
                          .replace("let tl =", f"let tl_{tag} ="))
        # 形参化模板里的引用同步替换
        fixed = []
        for s in stmts:
            for v in ("tv", "mr", "ms", "tl"):
                s = re.sub(rf"\b{v}\b", f"{v}_{tag}", s)
            fixed.append(s)
        return list(ov.get("uses_extra", [])), fixed
    args = ov.get("args") or auto_args(func["params"])
    if args is None:
        raise ValueError(f"no auto args for {name}{func['params']}")
    call = f"{name}({args})"
    shape = ov.get("ret_shape")
    if shape is None:
        shape = classify_ret(func["ret"])
    uses = list(ov.get("uses_extra", []))
    if shape == "unit":
        return uses, [f"{call};"]
    if shape == "print":
        return uses, [f"println({call});"]
    if shape == "result":
        return uses, [
            f"match {call} {{ Ok(v) => {{ println(v) }} Err(e) => {{ println(e) }} }}",
        ]
    if shape == "option":
        return uses, [
            f"match {call} {{ Some(v) => {{ println(v) }} None => {{ println(-777777) }} }}",
        ]
    if shape == "tuple":
        return uses, [
            f"let tv_{tag} = {call};",
            f"println(tv_{tag}.0);",
            f"println(tv_{tag}.1);",
        ]
    if shape == "record":
        return uses, [
            f"let mr_{tag} = {call};",
            f"println(size(mr_{tag}));",
        ]
    if shape == "set":
        return uses, [
            f"let ms_{tag} = {call};",
            f"println(set_size(ms_{tag}));",
        ]
    raise ValueError(f"unknown ret shape {shape} for {name}")


def classify_ret(ret: str | None) -> str:
    if ret is None or ret == "()":
        return "unit"
    ret = re.sub(r"\s+where\s+.*$", "", ret).strip()
    if ret in PRINTABLE_SCALARS or ret in ("T", "U", "V"):
        return "print"
    if ret.startswith("Result<"):
        return "result"
    if ret.startswith("Option<"):
        return "option"
    if ret.startswith("("):
        return "tuple"
    if ret == "Record":
        return "record"
    if ret == "Set" or ret.startswith("Set<"):
        return "set"
    if ret == "List" or ret.startswith("List<"):
        return "print"
    raise ValueError(f"unclassified ret: {ret}")


def gen_single(mod: str, funcs: list[dict]) -> str:
    uses = [f"use std::{mod}"]
    body: list[str] = []
    skipped = []
    for fn in funcs:
        key = f"{mod}.{fn['name']}"
        ov = OVERRIDES.get(key)
        if ov and ov.get("skip"):
            skipped.append(f"// SKIP {fn['name']}: {ov['skip']}")
            continue
        if ov is None and any(t not in DEFAULT_ARGS for _n, t in fn["params"]):
            skipped.append(
                f"// TODO-UNCOVERED {fn['name']}{[(n, t) for n, t in fn['params']]}"
            )
            continue
        try:
            u2, stmts = emit_call(fn, ov or {})
        except ValueError as e:
            skipped.append(f"// TODO-UNCOVERED {fn['name']}: {e}")
            continue
        uses.extend(f"use std::{m}" for m in u2)
        body.append(f"// {key}")
        body.extend(stmts)
    lines = ["// AUTO-GENERATED by scripts/gen-stdlib-matrix.py — do not edit by hand"]
    lines.extend(dict.fromkeys(uses))
    lines.append("")
    lines.extend(skipped)
    lines.append("func main() {")
    for b in body:
        lines.append("    " + b)
    lines.append("}")
    return "\n".join(lines) + "\n"


def gen_pair(a: str, b: str) -> str:
    uses, body = [], []
    for mod in (a, b):
        extra = PAIR_USES_EXTRA.get(mod, [])
        uses.append(f"use std::{mod}")
        uses.extend(f"use std::{m}" for m in extra)
        body.append(f"// --- {mod} ---")
        body.append(PAIR_REP[mod])
    lines = ["// AUTO-GENERATED pairwise merge probe"]
    lines.extend(dict.fromkeys(uses))
    lines.append("")
    lines.append("func main() {")
    for bl in body:
        lines.append("    " + bl)
    lines.append("}")
    return "\n".join(lines) + "\n"


def gen_trap(mod: str, fname: str, call: str) -> str:
    return (
        f"// AUTO-GENERATED trap-parity probe: {mod}.{fname}\n"
        f"use std::{mod}\n\n"
        f"func main() {{\n    {call}\n}}\n"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tests/stdlib_matrix_generated")
    args = ap.parse_args()
    out = REPO / args.out
    mods = load_modules()
    (out / "singles").mkdir(parents=True, exist_ok=True)
    (out / "pairs").mkdir(parents=True, exist_ok=True)
    (out / "traps").mkdir(parents=True, exist_ok=True)

    n_single = n_pair = n_trap = 0
    uncovered: list[str] = []
    for mod, funcs in mods.items():
        text = gen_single(mod, funcs)
        (out / "singles" / f"{mod}.mimi").write_text(text)
        n_single += 1
        for line in text.splitlines():
            if line.startswith("// TODO-UNCOVERED"):
                uncovered.append(line)
    for a, b in itertools.combinations(sorted(mods), 2):
        (out / "pairs" / f"{a}__{b}.mimi").write_text(gen_pair(a, b))
        n_pair += 1
    mega_body = []
    mega_uses = []
    for mod in sorted(mods):
        extra = PAIR_USES_EXTRA.get(mod, [])
        mega_uses.append(f"use std::{mod}")
        mega_uses.extend(f"use std::{m}" for m in extra)
        mega_body.append(f"// --- {mod} ---")
        mega_body.append(PAIR_REP[mod])
    mega = ["// AUTO-GENERATED mega merge probe (all modules)"]
    mega.extend(dict.fromkeys(mega_uses))
    mega.append("")
    mega.append("func main() {")
    mega.extend("    " + mb for mb in mega_body)
    mega.append("}")
    (out / "mega.mimi").write_text("\n".join(mega) + "\n")
    for (mod, fname), call in TRAP_FUNCS.items():
        (out / "traps" / f"{mod}_{fname}.mimi").write_text(gen_trap(mod, fname, call))
        n_trap += 1

    print(f"singles={n_single} pairs={n_pair} mega=1 traps={n_trap}")
    if uncovered:
        print(f"-- uncovered ({len(uncovered)}):")
        for u in uncovered:
            print("  " + u)
    return 0


if __name__ == "__main__":
    sys.exit(main())
