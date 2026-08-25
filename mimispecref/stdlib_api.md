# Mimi Standard Library API Reference

> Auto-generated from `mimi/std/*.mimi`. Do not edit manually.


> **379 public functions + constants across 23 modules.**


## `array` (16)

- `pub func array_new(n: i32, default: string) -> List<string>` — array_new: Allocates a list of length n with every element set to default.
- `pub func array_len(arr: List<string>) -> i32` — array_len: Returns the number of elements.
- `pub func array_get(arr: List<string>, index: i32) -> string` — array_get: Returns the element at index. Returns empty string if out of bounds.
- `pub func array_set(arr: List<string>, index: i32, value: string) -> List<string>` — array_set: Returns a new list with the element at index replaced.
- `pub func array_fill(arr: List<string>, value: string) -> List<string>` — array_fill: Returns a new list with every element replaced by value.
- `pub func array_slice(arr: List<string>, start: i32, end: i32) -> List<string>` — array_slice: Returns elements in [start, end). Clamps to valid range.
- `pub func array_reverse(arr: List<string>) -> List<string>` — array_reverse: Returns a new list with elements in reversed order.
- `pub func array_rotate_left(arr: List<string>, n: i32) -> List<string>` — array_rotate_left: Returns a new list rotated left by n positions. Positive n rotates elements toward index 0.
- `pub func array_rotate_right(arr: List<string>, n: i32) -> List<string>` — array_rotate_right: Returns a new list rotated right by n positions.
- `pub func array_binary_search(arr: List<string>, target: string) -> i32` — array_binary_search: Performs a binary search for target in a sorted ascending list. Returns the index of target, or -1 if not found. Precondition: arr must be sorted ascending.
- `pub func array_index_of(arr: List<string>, target: string) -> i32` — array_index_of: Linear search. Returns the first index where arr[i] == target, or -1.
- `pub func array_contains(arr: List<string>, target: string) -> bool` — array_contains: Returns true if target appears in arr.
- `pub func array_equals(a: List<string>, b: List<string>) -> bool` — array_equals: Returns true if both lists have the same length and element-wise equality.
- `pub func array_concat(a: List<string>, b: List<string>) -> List<string>` — array_concat: Returns a + b (concatenation).
- `pub func array_take(arr: List<string>, n: i32) -> List<string>` — array_take: Returns the first n elements (or all if n > len).
- `pub func array_drop(arr: List<string>, n: i32) -> List<string>` — array_drop: Returns all elements after the first n.

## `collections` (42)

- `pub func is_empty_list<T>(xs: List<T>) -> bool` — is_empty_list: Returns true if xs has length 0.
- `pub func first<T>(xs: List<T>) -> T` — first: Returns the first element of xs.
- `pub func first_result<T>(xs: List<T>) -> Result<T, string>` — first_result: Returns the first element of xs, or Err if xs is empty.
- `pub func last<T>(xs: List<T>) -> T` — last: Returns the last element of xs.
- `pub func last_result<T>(xs: List<T>) -> Result<T, string>` — last_result: Returns the last element of xs, or Err if xs is empty.
- `pub func take<T>(xs: List<T>, n: i32) -> List<T>` — take: Returns the first n elements of xs ([] when n <= 0).
- `pub func drop_n<T>(xs: List<T>, n: i32) -> List<T>` — drop_n: Returns xs without the first n elements (full xs when n <= 0).
- `pub func find<T>(xs: List<T>, target: T) -> (bool, i32)` — find: Searches for target in xs, returns (found, index).
- `pub func count_val<T>(xs: List<T>, target: T) -> i32` — count_val: Counts occurrences of target in xs.
- `pub func dedup<T>(xs: List<T>) -> List<T>` — dedup: Removes duplicate elements from xs (stable order).
- `pub func concat<T>(xs: List<T>, ys: List<T>) -> List<T>` — concat: Appends ys to xs.
- `pub func reverse_list<T>(xs: List<T>) -> List<T>` — reverse_list: Returns xs in reverse order.
- `pub func remove_at<T>(xs: List<T>, index: i32) -> List<T>` — remove_at: Returns xs with the element at index removed.
- `pub func remove_value<T>(xs: List<T>, value: T) -> List<T>` — remove_value: Returns xs with all occurrences of value removed.
- `pub func fill_list<T>(n: i32, value: T) -> List<T>` — fill_list: Creates a list of length n filled with value.
- `pub func range_step(start: i32, end: i32, step: i32) -> List<i32>` — range_step: Generates a range [start, end) with given step.
- `pub func sum(xs: List<i32>) -> i32` — sum: Sums a list of integers.
- `pub func sum_float(xs: List<f64>) -> f64` — sum_float: Sums a list of floats.
- `pub func flatten<T>(xss: List<List<T>>) -> List<T>` — flatten: Flattens a list of lists into a single list.
- `pub func sort_list(xs: List<i32>) -> List<i32>` — sort_list: Returns a sorted copy of xs (ascending).
- `pub func sort_list_f64(xs: List<f64>) -> List<f64>` — sort_list_f64: Returns a sorted copy of xs (ascending) for f64 lists.
- `pub func sort_list_str(xs: List<string>) -> List<string>` — sort_list_str: Returns a sorted copy of xs (ascending) for string lists.
- `pub func filter_positives(xs: List<i32>) -> List<i32>` — filter_positives: Filters xs to keep only positive values.
- `pub func filter_negatives(xs: List<i32>) -> List<i32>` — filter_negatives: Filters xs to keep only negative values.
- `pub func filter_evens(xs: List<i32>) -> List<i32>` — filter_evens: Filters xs to keep only even values.
- `pub func filter_odds(xs: List<i32>) -> List<i32>` — filter_odds: Filters xs to keep only odd values.
- `pub func map_list<T, U>(xs: List<T>, f: func(T) -> U) -> List<U>` — map_list: Applies f to each element of xs.
- `pub func unique<T>(xs: List<T>) -> List<T>` — unique: Returns xs with duplicate elements removed (alias for dedup).
- `pub func any<T>(xs: List<T>, pred: func(T) -> bool) -> bool` — any: Returns true if any element satisfies pred.
- `pub func all<T>(xs: List<T>, pred: func(T) -> bool) -> bool` — all: Returns true if all elements satisfy pred.
- `pub func find_map<T, U>(xs: List<T>, f: func(T) -> (bool, U)) -> Option<U>` — find_map: Finds first element matching f, returns Some(mapped) or None.
- `pub func partition<T>(xs: List<T>, pred: func(T) -> bool) -> (List<T>, List<T>)` — partition: Splits xs into (matching, non-matching) per pred.
- `pub func chunks<T>(xs: List<T>, size: i32) -> List<List<T>>` — chunks: Splits xs into chunks of size (last chunk may be smaller).
- `pub func intersperse<T>(xs: List<T>, sep: T) -> List<T>` — intersperse: Returns xs with sep placed between each element.
- `pub func min_list(xs: List<i32>) -> i32` — min_list: Returns the minimum element of xs (integer).
- `pub func min_list_result(xs: List<i32>) -> Result<i32, string>` — min_list_result: Returns the minimum element of xs (integer), or Err if empty.
- `pub func max_list(xs: List<i32>) -> i32` — max_list: Returns the maximum element of xs (integer).
- `pub func max_list_result(xs: List<i32>) -> Result<i32, string>` — max_list_result: Returns the maximum element of xs (integer), or Err if empty.
- `pub func filter_list<T>(xs: List<T>, pred: func(T) -> bool) -> List<T>` — filter_list: Filters xs by predicate (custom implementation).
- `pub func reduce_list<T, U>(xs: List<T>, f: func(U, T) -> U, init: U) -> U` — reduce_list: Reduces xs with f and initial value init. Generic over any element type T (unlike builtin `reduce` which only supports i64 elements in codegen).
- `pub func min_index(xs: List<i32>) -> i32` — min_index: Returns the index of the minimum element (-1 if empty).
- `pub func max_index(xs: List<i32>) -> i32` — max_index: Returns the index of the maximum element (-1 if empty).

## `crypto` (5)

- `pub func is_valid_hex(s: string) -> bool` — is_valid_hex: Returns true if s is valid hexadecimal.
- `pub func hex_encode(s: string) -> string` — hex_encode: Converts a string to its hex representation. Each Unicode scalar is emitted as UTF-8 bytes (two hex digits each). Nibbles are always clamped to 0..15 so high code points never OOB.
- `pub func hex_code_utf8(code: i64) -> string` — Encodes one Unicode scalar as UTF-8 hex bytes; exported until module-private dependency lowering is available.
- `pub func hex_byte(b: i64) -> string` — Encodes one byte as two lowercase hex digits.
- `pub func hex_digit(n: i64) -> string` — Converts a clamped nibble to one lowercase hex digit.

## `csv` (5)

- `pub func parse(csv_str: string) -> List<List<string>>` — parse: Splits CSV into rows (each row is a list of fields). Does NOT handle quoted fields with embedded commas — use a dedicated CSV library for production use.
- `pub func serialize_field(field: string) -> string` — serialize_field: Escapes a single CSV field (quotes commas, quotes, newlines).
- `pub func serialize_row(row: List<string>) -> string` — serialize_row: Serializes fields into CSV line.
- `pub func serialize(rows: List<List<string>>) -> string` — serialize: Serializes rows into CSV string.
- `pub func cell(rows: List<List<string>>, row: i32, col: i32) -> string` — cell: Gets cell at (row, col). Returns "" if out of bounds. 0.39.137: renamed from `get` — the bare name collided with maps::get (fail-loud duplicate item when both modules are imported) and the semantics differ entirely (positional cell lookup vs keyed map access).

## `datetime` (25)

- `pub func now_secs() -> i64` — now_secs: Current Unix timestamp in seconds. Duplicate of time::timestamp — kept for compatibility.
- `pub func now_millis() -> i64` — now_millis: Current Unix timestamp in milliseconds. Duplicate of time::timestamp_ms — kept for compatibility.
- `pub func timestamp_secs() -> i64` — timestamp_secs: Alias for now_secs (and time::timestamp).
- `pub func timestamp_millis() -> i64` — timestamp_millis: Alias for now_millis (and time::timestamp_ms).
- `pub func seconds_to_millis(secs: i64) -> i64` — seconds_to_millis: Converts seconds to milliseconds.
- `pub func millis_to_seconds(ms: i64) -> i64` — millis_to_seconds: Converts milliseconds to seconds.
- `pub func format_duration_secs(total_secs: i64) -> string` — format_duration_secs: Formats total_secs as "Xd Yh Zm Ws".
- `pub func format_duration_ms(ms: i64) -> string` — format_duration_ms: Formats ms as duration with fractional seconds.
- `pub func days_from_now(days: i64) -> i64` — days_from_now: Returns Unix timestamp days from now.
- `pub func hours_from_now(hours: i64) -> i64` — hours_from_now: Returns Unix timestamp hours from now.
- `pub func minutes_from_now(mins: i64) -> i64` — minutes_from_now: Returns Unix timestamp minutes from now.
- `pub func is_future(timestamp_secs: i64) -> bool` — is_future: Returns true if timestamp_secs > now.
- `pub func is_past(timestamp_secs: i64) -> bool` — is_past: Returns true if timestamp_secs < now.
- `pub func time_since(timestamp_secs: i64) -> i64` — time_since: Returns seconds elapsed since timestamp_secs.
- `pub func time_until(timestamp_secs: i64) -> i64` — time_until: Returns seconds until timestamp_secs.
- `pub func elapsed_since(start_ms: i64) -> i64` — elapsed_since: Returns milliseconds elapsed since start_ms. Duplicate of time::elapsed — kept for compatibility.
- `pub func sleep_secs(secs: i32)` — sleep_secs: Sleeps for secs seconds.
- `pub func sleep_until(timestamp_secs: i64)` — sleep_until: Sleeps until Unix timestamp_secs.
- `pub const SECONDS_PER_MINUTE = 60` — Time constants
- `pub const SECONDS_PER_HOUR = 3600`
- `pub const SECONDS_PER_DAY = 86400`
- `pub const MILLIS_PER_SECOND = 1000`
- `pub const MILLIS_PER_MINUTE = 60000`
- `pub const MILLIS_PER_HOUR = 3600000`
- `pub const MILLIS_PER_DAY = 86400000`

## `env` (9)

- `pub func get_var(name: string) -> Result<string, string>` — get_var: Returns the value of environment variable name, or Err if not set.
- `pub func cli_args() -> List<string>` — cli_args: Returns the command-line arguments.
- `pub func get_var_or(name: string, default: string) -> string` — get_var_or: Returns env var value or default if not set.
- `pub func has_var(name: string) -> bool` — has_var: Returns true if the environment variable is set.
- `pub func get_int(name: string) -> Result<i32, string>` — get_int: Reads and parses env var as i32. Returns Err if missing or invalid.
- `pub func get_float(name: string) -> Result<f64, string>` — get_float: Reads and parses env var as f64. Returns Err if missing or invalid.
- `pub func arg_count() -> i32` — arg_count: Returns the number of CLI arguments.
- `pub func first_arg() -> string` — first_arg: Returns the first CLI argument, or "" if none.
- `pub func set_var(key: string, value: string) -> bool` — set_var: Sets an environment variable. Returns true on success.

## `errors` (6)

- `pub func fs_error_to_string(e: FsError) -> string` — fs_error_to_string: Convert FsError to a human-readable string.
- `pub func json_error_to_string(e: JsonError) -> string` — json_error_to_string: Convert JsonError to a human-readable string.
- `pub func collection_error_to_string(e: CollectionError) -> string` — collection_error_to_string: Convert CollectionError to a human-readable string.
- `pub func net_error_to_string(e: NetError) -> string` — net_error_to_string: Convert NetError to a human-readable string.
- `pub func math_error_to_string(e: MathError) -> string` — math_error_to_string: Convert MathError to a human-readable string.
- `pub func app_error_to_string(e: AppError) -> string` — app_error_to_string: Convert AppError to a human-readable string.

## `fs` (12)

- `pub func exists(path: string) -> bool` — exists: Returns true if path exists on disk.
- `pub func read(path: string) -> Result<string, string>` — read: Reads the entire file at path into a string. Returns Err if the file cannot be read.
- `pub func write(path: string, content: string) -> Result<(), string>` — write: Writes content to path, overwriting if it exists. Returns Err if the write fails.
- `pub func read_lines(path: string) -> Result<List<string>, string>` — read_lines: Reads file at path and splits by newline. Returns Err with original I/O error message if the file cannot be read.
- `pub func file_size(path: string) -> Result<i64, string>` — file_size: Returns the size of the file at path in bytes, or Err if it cannot be stat'ed. The size is an i64 so files larger than 2 GiB are not truncated.
- `pub func write_lines(path: string, xs: List<string>) -> Result<(), string>` — write_lines: Writes each string in xs as a line to path. Returns Err if the write fails.
- `pub func stat(path: string) -> StatResult` — stat: Returns file metadata (size, modified timestamp, is_file, is_dir). Returns size=-1 if the file does not exist.
- `pub func append(path: string, content: string) -> bool` — append: Appends content to a file. Creates the file if it does not exist. Returns true on success, false on failure.
- `pub func read_partial(path: string, max_bytes: i32) -> string` — read_partial: Reads up to max_bytes from a file into a string. Useful for large files to avoid loading everything into memory. NOTE (batch5-04 P2-6): the underlying builtin currently returns "" on read/stat failure; this API intentionally keeps that behavior for compatibility. New code should prefer `read_file`/`read` when error visibility is required.
- `pub func read_bytes(path: string) -> string` — read_bytes: Reads an entire file as raw bytes (lossy UTF-8 conversion). For binary data, prefer read_partial for large files. NOTE (batch5-04 P2-6): the underlying builtin currently returns "" on read/stat failure; this API intentionally keeps that behavior for compatibility. New code should prefer `read_file`/`read` when error visibility is required.
- `pub func write_bytes(path: string, data: string) -> bool` — write_bytes: Writes raw byte data to a file. Returns true on success, false on failure. NOTE (batch5-04 P2-6): false currently gives no diagnostic string; use `write_file` when you need the underlying error message.
- `pub func read_lines_json(path: string) -> string` — read_lines_json: Reads file line-by-line and returns a JSON array of lines. More memory-efficient than read_file + split for very large files. NOTE (batch5-04 P2-6): the underlying builtin currently returns "[]" on read failure; this API intentionally keeps that behavior for compatibility.

## `io` (13)

- `pub func print_line(s: string)` — print_line: Prints s followed by a newline to stdout.
- `pub func print_raw(s: string)` — print_raw: Prints s to stdout without a trailing newline.
- `pub func print_format(parts: List<string>)` — print_format: Joins parts and prints without separator.
- `pub func print_err(s: string)` — print_err: Prints s followed by a newline to stderr.
- `pub func print_lines(xs: List<string>)` — print_lines: Prints each element of xs on its own line.
- `pub func print_bool(b: bool)` — print_bool: Prints "true" or "false".
- `pub func print_int(n: i32)` — print_int: Prints n to stdout.
- `pub func print_float(f: f64)` — print_float: Prints f to stdout.
- `pub func print_list<T>(xs: List<T>)` — print_list: Prints xs to stdout using its Show representation.
- `pub func input_line() -> Result<string, string>` — input_line: Reads a line from stdin. Returns Err on EOF/read error; a successfully read empty line is Ok("").
- `pub func input_int() -> Result<i32, string>` — input_int: Reads a line from stdin and parses it as an integer.
- `pub func input_float() -> Result<f64, string>` — input_float: Reads a line from stdin and parses it as a float.
- `pub func input_bool() -> Result<bool, string>` — input_bool: Reads a line from stdin; accepts true/false/yes/no/1/0 (case-insensitive).

## `iter` (11)

- `pub func iter_range(start: i32, end: i32) -> List<string>` — iter_range: Returns the integers in [start, end) as stringified values. Example: iter_range(3, 7) -> ["3", "4", "5", "6"]
- `pub func iter_zip(a: List<string>, b: List<string>) -> List<string>` — iter_zip: Pairs the i-th element of a with the i-th element of b. Result is a list of "a|b" strings, truncated to the shorter input.
- `pub func iter_enumerate(arr: List<string>) -> List<string>` — iter_enumerate: Returns a list of "i|x" strings representing (index, value) pairs. Example: iter_enumerate(["a", "b"]) -> ["0|a", "1|b"].
- `pub func iter_take(arr: List<string>, n: i32) -> List<string>` — iter_take: Returns the first n elements (or all if n > len).
- `pub func iter_drop(arr: List<string>, n: i32) -> List<string>` — iter_drop: Returns all elements after the first n.
- `pub func iter_take_while(arr: List<string>, pred_value: string) -> List<string>` — iter_take_while: Returns elements from the start while pred(value) is true. `pred` is represented as a prefix string that is matched against each element: if the element starts with pred, it is taken. (Mimi's lambda capture is limited; this pragmatic version supports equality-based filtering.)
- `pub func iter_chain(a: List<string>, b: List<string>) -> List<string>` — iter_chain: Concatenates a and b.
- `pub func iter_repeat(value: string, n: i32) -> List<string>` — iter_repeat: Returns a list with `value` repeated n times.
- `pub func iter_reversed(arr: List<string>) -> List<string>` — iter_reversed: Returns elements in reverse order.
- `pub func iter_count(arr: List<string>, value: string) -> i32` — iter_count: Returns the number of elements equal to value.
- `pub func iter_unique(arr: List<string>) -> List<string>` — iter_unique: Returns a new list with duplicates removed (first occurrence wins).

## `json` (8)

- `pub func get_string(json: string, key: string) -> string` — get_string: Gets a string field from a JSON object by key.
- `pub func get_element(json: string, index: i32) -> string` — get_element: Gets the element at index from a JSON array.
- `pub func get_bool(json: string, key: string) -> Result<bool, string>` — get_bool: Gets a boolean field from a JSON object by key. Returns Ok(true/false) if key exists, Err if key missing or not boolean.
- `pub func is_valid_json(s: string) -> bool` — is_valid_json: Returns true if s is syntactically valid JSON.
- `pub func to_string_pretty(json: string) -> string` — to_string_pretty: Pretty-prints a JSON string with indentation. NOTE (batch5-04 P3): this is still a compatibility placeholder and does NOT actually reformat the JSON yet — it returns the input unchanged. Do not rely on it for pretty output; prefer `to_string` until a real pretty-printer is implemented.
- `pub func get_object(json: string, key: string) -> string` — get_object: Gets a nested JSON object by key as a string.
- `pub func get_array(json: string, key: string) -> string` — get_array: Gets a nested JSON array by key as a string.
- `pub func array_length(json: string) -> i32` — array_length: Returns the number of elements in a JSON array.

## `maps` (15)

- `pub func new() -> Record` — new: Creates an empty Record (map).
- `pub func get(m: Record, key: string) -> (bool, Any)` — get: Looks up key in map m. Returns (found, value).
- `pub func set(m: Record, key: string, value: Any) -> Record` — set: Sets key to value in map m, returns updated map. Preserves the original map on both VM and native backends by copying first.
- `pub func has_key(m: Record, key: string) -> bool` — has_key: Returns true if key exists in m.
- `pub func remove(m: Record, key: string) -> Record` — remove: Removes key from map m, returns updated map. Preserves the original map on both VM and native backends by copying first.
- `pub func size(m: Record) -> i32` — size: Returns the number of entries in map m.
- `pub func from_list(pairs: List<(string, Any)>) -> Record` — from_list: Creates a map from a list of (key, value) pairs.
- `pub func get_or_default(m: Record, key: string, default: Any) -> Any` — get_or_default: Looks up key, returns default if not found.
- `pub func merge(a: Record, b: Record) -> Record` — merge: Returns a new map with all entries from a and b (b overwrites a).
- `pub func to_list(m: Record) -> List<(string, Any)>` — to_list: Converts a map to a list of (key, value) pairs.
- `pub func filter_keys(m: Record, pred: func(string) -> bool) -> Record` — filter_keys: Returns a new map with only keys satisfying pred.
- `pub func map_values(m: Record, f: func(Any) -> Any) -> Record` — map_values: Returns a new map with f applied to each value.
- `pub func update(m: Record, key: string, f: func(Any) -> Any) -> Record` — update: Applies f to the value at key, returns updated map.
- `pub func pick(m: Record, ks: List<string>) -> Record` — pick: Returns a new map with only the specified keys.
- `pub func omit(m: Record, ks: List<string>) -> Record` — omit: Returns a new map without the specified keys.

## `mymath` (61)

- `pub func abs(x: i32) -> i32` — abs: Absolute value for integers.
- `pub func abs_float(x: f64) -> f64` — abs_float: Absolute value for floats.
- `pub func sign(x: i32) -> i32` — sign: Returns -1, 0, or 1 indicating sign of x.
- `pub func sign_float(x: f64) -> f64` — sign_float: Returns -1.0, 0.0, or 1.0 indicating sign of x.
- `pub func factorial(n: i32) -> i32` — factorial: Iterative factorial of n; returns -1 for n < 0 or n > 12 (13! exceeds i32::MAX).
- `pub func fibonacci(n: i32) -> i32` — fibonacci: Returns the nth Fibonacci number (F_0 = 0, F_1 = 1).
- `pub func is_prime(n: i32) -> bool` — is_prime: Returns true if n is a prime number.
- `pub func mod_pow(base: i32, exp: i32, modulus: i32) -> i32` — mod_pow: Modular exponentiation: (base^exp) % modulus.
- `pub func deg_to_rad(degrees: f64) -> f64` — deg_to_rad: Converts degrees to radians.
- `pub func rad_to_deg(radians: f64) -> f64` — rad_to_deg: Converts radians to degrees.
- `pub func map_range(value: f64, in_min: f64, in_max: f64, out_min: f64, out_max: f64) -> f64` — map_range: Maps value from [in_min, in_max] to [out_min, out_max].
- `pub func is_power_of_two(n: i32) -> bool` — is_power_of_two: Returns true if n is a power of two (n > 0).
- `pub func next_power_of_two(n: i32) -> i32` — next_power_of_two: Returns smallest power of two >= n.
- `pub func count_digits(n: i32) -> i32` — count_digits: Returns the number of decimal digits in n.
- `pub func digit_at(n: i32, pos: i32) -> i32` — digit_at: Returns the digit at position pos (0 = units) in n.
- `pub func sum_digits(n: i32) -> i32` — sum_digits: Returns the sum of decimal digits of n.
- `pub func reverse_number(n: i32) -> i32` — reverse_number: Returns n with decimal digits reversed. Uses i64 arithmetic to avoid overflow; returns -1 if the reversed value cannot be represented as i32.
- `pub func is_palindrome_number(n: i32) -> bool` — is_palindrome_number: Returns true if n is a palindrome (e.g. 121).
- `pub func collatz_steps(n: i32) -> i32` — collatz_steps: Returns steps to reach 1 via Collatz sequence.
- `pub func power(base: f64, exp: f64) -> f64` — power: Returns base^exp (floating-point).
- `pub func sqrt_val(x: f64) -> f64` — sqrt_val: Returns the square root of x.
- `pub func floor_val(x: f64) -> f64` — floor_val: Returns the largest integer <= x as f64.
- `pub func ceil_val(x: f64) -> f64` — ceil_val: Returns the smallest integer >= x as f64.
- `pub func round_val(x: f64) -> f64` — round_val: Returns x rounded to nearest integer as f64.
- `pub func gcd(a: i32, b: i32) -> i32` — gcd: Greatest common divisor of a and b (Euclidean algorithm); always non-negative.
- `pub func lcm(a: i32, b: i32) -> i32` — lcm: Least common multiple of a and b; always non-negative (0 if either is 0).
- `pub func is_divisible_by(n: i32, d: i32) -> bool` — is_divisible_by: Returns true if n is divisible by d (d != 0).
- `pub func min_val(a: i32, b: i32) -> i32` — min_val: Returns the smaller of a and b.
- `pub func max_val(a: i32, b: i32) -> i32` — max_val: Returns the larger of a and b.
- `pub func area_circle(r: f64) -> f64` — area_circle: Returns πr².
- `pub func circumference(r: f64) -> f64` — circumference: Returns 2πr.
- `pub func hypot(a: f64, b: f64) -> f64` — hypot: Returns sqrt(a² + b²).
- `pub func my_sin(x: f64) -> f64` — my_sin: Returns the sine of x (radians).
- `pub func my_cos(x: f64) -> f64` — my_cos: Returns the cosine of x (radians).
- `pub func my_tan(x: f64) -> f64` — my_tan: Returns the tangent of x (radians).
- `pub func my_asin(x: f64) -> f64` — my_asin: Returns the arcsine of x in [-1, 1], result in [-π/2, π/2].
- `pub func my_acos(x: f64) -> f64` — my_acos: Returns the arccosine of x in [-1, 1], result in [0, π].
- `pub func my_atan(x: f64) -> f64` — my_atan: Returns the arctangent of x, result in [-π/2, π/2].
- `pub func my_atan2(y: f64, x: f64) -> f64` — my_atan2: Two-argument arctangent: angle of (y, x) in radians.
- `pub func my_sinh(x: f64) -> f64` — my_sinh: Hyperbolic sine.
- `pub func my_cosh(x: f64) -> f64` — my_cosh: Hyperbolic cosine.
- `pub func my_tanh(x: f64) -> f64` — my_tanh: Hyperbolic tangent.
- `pub func my_ln(x: f64) -> f64` — my_ln: Natural logarithm (base e). Returns NaN for x < 0, -∞ for x = 0.
- `pub func my_log(x: f64, base: f64) -> f64` — my_log: Base-N logarithm: log(x, base).
- `pub func my_log2(x: f64) -> f64` — my_log2: Binary logarithm (base 2).
- `pub func my_log10(x: f64) -> f64` — my_log10: Common logarithm (base 10).
- `pub func my_exp(x: f64) -> f64` — my_exp: Returns e^x.
- `pub func my_exp2(x: f64) -> f64` — my_exp2: Returns 2^x.
- `pub func my_cbrt(x: f64) -> f64` — my_cbrt: Returns the real cube root of x.
- `pub func random_normal() -> f64` — random_normal: Standard normal sample using Box-Muller transform. Returns N(0, 1).
- `pub func random_uniform(lo: f64, hi: f64) -> f64` — random_uniform: Uniform sample in [lo, hi).
- `pub func random_exponential(lambda: f64) -> f64` — random_exponential: Sample from Exp(λ) (rate parameter λ > 0). Returns -ln(1 - u) / λ, with u uniform in [0, 1). Returns -1.0 (sentinel, never a valid sample) for λ <= 0.
- `pub func random_bernoulli(p: f64) -> bool` — random_bernoulli: Returns true with probability p, false otherwise.
- `pub func random_int_range(lo: i32, hi: i32) -> i32` — random_int_range: Returns an integer in [lo, hi) (uniform).
- `pub func try_div(a: i32, b: i32) -> Result<i32, string>` — try_div: Checked integer division. Returns Err("division by zero") if divisor is 0.
- `pub func try_mod(a: i32, b: i32) -> Result<i32, string>` — try_mod: Checked integer modulo. Returns Err("division by zero") if divisor is 0.
- `pub func try_factorial(n: i32) -> Result<i32, string>` — try_factorial: Checked factorial. Returns Err for n < 0 or n > 12 (13! exceeds i32::MAX).
- `pub func try_sqrt(x: f64) -> Result<f64, string>` — try_sqrt: Checked square root. Returns Err for negative input.
- `pub func try_ln(x: f64) -> Result<f64, string>` — try_ln: Checked natural logarithm. Returns Err for x <= 0.
- `pub func try_log(x: f64, base: f64) -> Result<f64, string>` — try_log: Checked logarithm with base. Returns Err for invalid inputs.
- `pub func try_pow_int(base: i32, exp: i32) -> Result<i32, string>` — try_pow_int: Checked integer power. Returns Err if result would overflow.

## `net` (14)

- `pub func tcp_socket() -> i64` — tcp_socket: Creates a TCP socket. Returns fd or negative on error.
- `pub func tcp_connect(host: string, port: i32) -> Result<i64, NetError>` — tcp_connect: Connects to host:port over TCP. Returns fd on success.
- `pub func tcp_listen(port: i32, backlog: i32) -> Result<i64, NetError>` — tcp_listen: Listens on port with backlog. Returns fd on success.
- `pub func tcp_accept(fd: i64) -> Result<i64, NetError>` — tcp_accept: Accepts a connection on a listening socket fd.
- `pub func tcp_send(fd: i64, data: string) -> Result<i64, NetError>` — tcp_send: Sends data over socket fd. Returns bytes sent on success.
- `pub func tcp_recv(fd: i64, buf_size: i32) -> Result<string, NetError>` — tcp_recv: Receives up to buf_size bytes from socket fd (Ok("") means EOF).
- `pub func fetch(url: string) -> Result<string, NetError>` — fetch: Performs HTTP GET, returns body string on success (possibly empty).
- `pub func fetch_post(url: string, body: string) -> Result<string, NetError>` — fetch_post: Performs HTTP POST with body, returns response on success (possibly empty).
- `pub const AF_INET: i32 = 2` — Socket address families
- `pub const AF_INET6: i32 = 10`
- `pub const SOCK_STREAM: i32 = 1` — Socket types
- `pub const SOCK_DGRAM: i32 = 2`
- `pub const IPPROTO_TCP: i32 = 6` — Protocol constants
- `pub const IPPROTO_UDP: i32 = 17`

## `prelude` (42)

- `pub func identity<T>(x: T) -> T` — identity: Returns the input unchanged.
- `pub func const_val<T, U>(x: T, _y: U) -> T` — const_val: Ignores second argument, returns first.
- `pub func is_even(x: i32) -> bool` — is_even: Returns true if x is divisible by 2.
- `pub func is_odd(x: i32) -> bool` — is_odd: Returns true if x is not divisible by 2.
- `pub func is_positive(x: i32) -> bool` — is_positive: Returns true if x > 0.
- `pub func is_negative(x: i32) -> bool` — is_negative: Returns true if x < 0.
- `pub func is_zero(x: i32) -> bool` — is_zero: Returns true if x == 0.
- `pub func negate(b: bool) -> bool` — negate: Logical NOT for booleans.
- `pub func min3(a: i32, b: i32, c: i32) -> i32` — min3: Returns the smallest of three integers.
- `pub func max3(a: i32, b: i32, c: i32) -> i32` — max3: Returns the largest of three integers.
- `pub func swap<T, U>(a: T, b: U) -> (U, T)` — swap: Swaps the positions of two values.
- `pub func clamp(value: i32, min_val: i32, max_val: i32) -> i32` — clamp: Restricts value to [min_val, max_val] inclusive.
- `pub func clamp_float(value: f64, min_val: f64, max_val: f64) -> f64` — clamp_float: Restricts floating-point value to [min_val, max_val] inclusive.
- `pub func lerp(a: f64, b: f64, t: f64) -> f64` — lerp: Linear interpolation: a + (b - a) * t.
- `pub func compose<T, U, V>(f: func(U) -> V, g: func(T) -> U) -> func(T) -> V` — compose: Returns f ∘ g (function composition).
- `pub func pipe<T, U>(x: T, f: func(T) -> U) -> U` — pipe: Applies f to x (pipeline operator).
- `pub func tap<T>(x: T, f: func(T) -> ()) -> T` — tap: Calls f(x) for side effects, then returns x.
- `pub func flip<T, U, V>(f: func(T, U) -> V) -> func(U, T) -> V` — flip: Returns a function with the first two arguments swapped.
- `pub func apply<T, U>(f: func(T) -> U, x: T) -> U` — apply: Alias for calling f(x).
- `pub func konst<T, U>(x: T) -> func(U) -> T` — konst: Returns a function that always returns x regardless of argument.
- `pub func fail(msg: string) -> ()` — fail: Prints an error message and exits with code 1.
- `pub func unreachable() -> ()` — unreachable: Prints error and exits — marks unreachable code paths.
- `pub func todo() -> ()` — todo: Prints message and exits — marks unimplemented code paths.
- `pub func default_i32() -> i32` — default_i32: Returns 0.
- `pub func default_f64() -> f64` — default_f64: Returns 0.0.
- `pub func default_bool() -> bool` — default_bool: Returns false.
- `pub func default_string() -> string` — default_string: Returns empty string.
- `pub func increment(x: i32) -> i32` — increment: Returns x + 1.
- `pub func decrement(x: i32) -> i32` — decrement: Returns x - 1.
- `pub func double(x: i32) -> i32` — double: Returns x * 2.
- `pub func triple(x: i32) -> i32` — triple: Returns x * 3.
- `pub func half(x: i32) -> i32` — half: Returns x / 2 (integer division).
- `pub func sqr(x: i32) -> i32` — sqr: Returns x * x.
- `pub func cube_int(x: i32) -> i32` — cube_int: Returns x * x * x.
- `pub func eq<T>(a: T, b: T) -> bool where T: Eq` — eq: Equality check via the Eq trait.
- `pub func not_eq<T>(a: T, b: T) -> bool where T: Eq` — not_eq: Inequality check via the Eq trait.
- `pub func bool_not(b: bool) -> bool` — not: Boolean NOT.
- `pub func assert_msg(cond: bool, msg: string)` — assert_msg: If condition is false, prints message and exits with code 1.
- `pub func repeat_action(n: i32, f: func(i32) -> ())` — repeat_action: Calls f(i) for i in 0..n.
- `pub func times(n: i32, f: func() -> ())` — times: Calls f() n times.
- `pub func to_int_safe(s: string, default: i32) -> i32` — to_int_safe: Parses s as integer, returns default on failure.
- `pub func to_float_safe(s: string, default: f64) -> f64` — to_float_safe: Parses s as float, returns default on failure.

## `random` (7)

- `pub func random_float(lo: f64, hi: f64) -> f64` — random_float: Returns random float in [lo, hi).
- `pub func random_int(lo: i32, hi: i32) -> i32` — random_int: Returns random integer in [lo, hi). Duplicate of mymath::random_int — kept for compatibility.
- `pub func random_bool() -> bool` — random_bool: Returns true with 50% probability.
- `pub func random_remove_ith<T>(xs: List<T>, i: i32) -> List<T>` — random_remove_ith: Returns a new list with the i-th element removed. It is a pub free helper so it survives the std module loader's pub-only merge (private helpers are invisible to consumers). Keeping the remove-at loop in a free generic function also avoids the codegen stack-alloca aliasing bug seen when the loop is inlined directly inside generic impl methods (shuffle/random_sample previously corrupted sh_rest by reusing the same list alloca for the next sh_kept).
- `pub func random_choice<T>(xs: List<T>) -> Result<T, string>` — random_choice: Returns a random element from xs, or Err if xs is empty.
- `pub func random_sample<T>(xs: List<T>, n: i32) -> List<T>` — random_sample: Returns n random elements from xs without replacement.
- `pub func shuffle<T>(xs: List<T>) -> List<T>` — shuffle: Returns xs with elements randomly permuted (Fisher-Yates).

## `result` (7)

- `pub func is_ok_result<T, E>(result: Result<T, E>) -> bool` — is_ok_result: Returns true if result is Ok.
- `pub func is_err_result<T, E>(result: Result<T, E>) -> bool` — is_err_result: Returns true if result is Err.
- `pub func result_unwrap<T, E>(result: Result<T, E>) -> T` — result_unwrap: Returns the Ok value or panics on Err.
- `pub func unwrap_or<T, E>(result: Result<T, E>, default: T) -> T` — unwrap_or: Returns the Ok value, or default on Err.
- `pub func expect_result<T, E>(result: Result<T, E>, msg: string) -> T` — expect_result: Returns the Ok value, or panics with msg on Err.
- `pub func map_result<T, E, U>(result: Result<T, E>, f: func(T) -> U) -> Result<U, E>` — map_result: Applies f to the Ok value, preserving Err.
- `pub func map_err_result<T, E, F>(result: Result<T, E>, f: func(E) -> F) -> Result<T, F>` — map_err_result: Applies f to the Err value, preserving Ok.

## `set` (7)

- `pub func insert(s: Set, value: Any) -> Set` — insert: Returns a set with value added.
- `pub func set_size(s: Set) -> i32` — set_size: Number of elements in the set.
- `pub func set_is_empty(s: Set) -> bool` — set_is_empty: True when the set has no elements.
- `pub func set_contains(s: Set, value: Any) -> bool` — set_contains: True when value is a member of the set.
- `pub func set_insert(s: Set, value: Any) -> Set` — set_insert: Returns a set with value added.
- `pub func set_remove(s: Set, value: Any) -> Set` — set_remove: Returns a set with value removed.
- `pub func set_to_list(s: Set) -> List` — set_to_list: Converts the set to a list of elements.

## `strings` (48)

- `pub func is_empty(s: string) -> bool` — is_empty: Returns true if s has length 0.
- `pub func char_at(s: string, index: i32) -> string` — char_at: Returns the character at index as a single-character string.
- `pub func substring(s: string, start: i32, end: i32) -> string` — substring: Returns s[start..end] inclusive-exclusive range.
- `pub func starts_with(s: string, prefix: string) -> bool` — starts_with: Returns true if s starts with prefix.
- `pub func ends_with(s: string, suffix: string) -> bool` — ends_with: Returns true if s ends with suffix.
- `pub func to_upper(s: string) -> string` — to_upper: Returns s converted to uppercase (ASCII).
- `pub func to_lower(s: string) -> string` — to_lower: Returns s converted to lowercase (ASCII).
- `pub func trim(s: string) -> string` — trim: Removes leading and trailing whitespace from s.
- `pub func replace(s: string, from: string, to: string) -> string` — replace: Replaces all occurrences of from with to in s.
- `pub func split(s: string, delimiter: string) -> List<string>` — split: Splits s by delimiter into a list of substrings.
- `pub func join(parts: List<string>, separator: string) -> string` — join: Combines parts with separator between each element.
- `pub func repeat(s: string, n: i32) -> string` — repeat: Returns s repeated n times. Non-positive n is defined as an empty string, never passed to the builtin.
- `pub func index_of(s: string, sub: string) -> Option<i32>` — index_of: Returns Some(pos) if sub is found in s, else None.
- `pub func parse_int(s: string) -> (bool, i64)` — parse_int: Parses s as integer. Returns (true, val) on success.
- `pub func parse_float(s: string) -> (bool, f64)` — parse_float: Parses s as float. Returns (true, val) on success.
- `pub func is_digit(s: string) -> bool` — is_digit: Returns true if s is a single ASCII digit (0-9).
- `pub func is_alpha(s: string) -> bool` — is_alpha: Returns true if s is a single ASCII letter (a-z, A-Z).
- `pub func is_alphanumeric(s: string) -> bool` — is_alphanumeric: Returns true if s is a digit or letter.
- `pub func count_char(s: string, target: string) -> i32` — count_char: Counts occurrences of target character in s.
- `pub func trim_left(s: string) -> string` — trim_left: Removes leading whitespace (space/tab/newline/CR) from s. The whitespace test is inlined (not a private helper): the std module loader only carries `pub` items across `use std::strings`, so a module-private helper is invisible to consumers (E0401) while pub bodies that call it are not. Loop+break form avoids relying on short-circuit `&&` for the bounds guard.
- `pub func trim_right(s: string) -> string` — trim_right: Removes trailing whitespace (space/tab/newline/CR) from s. Whitespace test inlined for the same loader reason as trim_left.
- `pub func pad_left(s: string, width: i32, pad_char: string) -> string` — pad_left: Pads s on the left with pad_char to reach width.
- `pub func pad_right(s: string, width: i32, pad_char: string) -> string` — pad_right: Pads s on the right with pad_char to reach width.
- `pub func lines(s: string) -> List<string>` — lines: Splits s by newline into a list of lines.
- `pub func words(s: string) -> List<string>` — words: Splits trimmed s by spaces into a list of non-empty words. Consecutive separators yield empty tokens; those are dropped so "a  b" is 2 words, not 3.
- `pub func contains(s: string, sub: string) -> bool` — contains: Returns true if sub appears in s.
- `pub func capitalize(s: string) -> string` — capitalize: Returns s with first character uppercased.
- `pub func title(s: string) -> string` — title: Returns s with each word capitalized.
- `pub func reverse_string(s: string) -> string` — reverse_string: Returns s with characters in reverse order.
- `pub func truncate(s: string, max_len: i32) -> string` — truncate: Returns s shortened to max_len with "..." suffix if needed. Non-positive max_len has no room for any characters, so return an empty string instead of relying on str_substring's negative/zero wrapping.
- `pub func remove_prefix(s: string, prefix: string) -> string` — remove_prefix: Strips prefix from s if present.
- `pub func remove_suffix(s: string, suffix: string) -> string` — remove_suffix: Strips suffix from s if present.
- `pub func count_lines(s: string) -> i32` — count_lines: Returns the number of lines in s.
- `pub func count_words(s: string) -> i32` — count_words: Returns the number of words in s.
- `pub func first_char(s: string) -> string` — first_char: Returns the first character of s, or "" if empty.
- `pub func last_char(s: string) -> string` — last_char: Returns the last character of s, or "" if empty.
- `pub func without_last(s: string) -> string` — without_last: Returns s without its last character.
- `pub func without_first(s: string) -> string` — without_first: Returns s without its first character.
- `pub func surround(s: string, left: string, right: string) -> string` — surround: Returns left + s + right.
- `pub func quote_str(s: string) -> string` — quote_str: Wraps s in double quotes.
- `pub func paren(s: string) -> string` — paren: Wraps s in parentheses.
- `pub func bracket(s: string) -> string` — bracket: Wraps s in square brackets.
- `pub func brace(s: string) -> string` — brace: Wraps s in curly braces.
- `pub func indent(s: string, n: i32) -> string` — indent: Prepends n spaces to each line of s. Non-positive n returns s unchanged (no negative repeat sent to builtin).
- `pub func ellipsis(s: string, max_len: i32) -> string` — ellipsis: Alias for truncate.
- `pub func count_substring(s: string, sub: string) -> i32` — count_substring: Counts non-overlapping occurrences of sub in s.
- `pub func is_blank(s: string) -> bool` — is_blank: Returns true if s is empty or whitespace-only.
- `pub func replace_all(s: string, from: string, to: string) -> string` — replace_all: Alias for replace.

## `template` (5)

- `pub func simple_render(template: string, vars: Record) -> string` — simple_render: Replaces {{key}} with values from vars. vars is a record where keys are variable names.
- `pub func render(template: string, lookup: func(string) -> string) -> string` — render: Replaces {{key}} with values from a lookup function.
- `pub func lookup_with_default(vars: Record, key: string, default_val: string) -> string` — Returns the string value for key, or default_val when the key is absent.
- `pub func render_with_defaults(template: string, vars: Record, default_val: string) -> string` — render_with_defaults: Like render, but returns default for unknown keys.
- `pub func render_csv(template: string, rows: List<List<string>>) -> string` — render_csv: Renders CSV rows using {{col_0}}, {{col_1}}, etc.

## `testing` (7)

- `pub func assert_eq_int(a: i32, b: i32)` — assert_eq_int: Asserts a == b, panics on mismatch.
- `pub func assert_ne_int(a: i32, b: i32)` — assert_ne_int: Asserts a != b, panics on mismatch.
- `pub func assert_approx_eq_float(a: f64, b: f64)` — assert_approx_eq_float: Asserts a ≈ b within floating tolerance.
- `pub func assert_true(cond: bool)` — assert_true: Asserts cond is true.
- `pub func assert_false(cond: bool)` — assert_false: Asserts cond is false.
- `pub func assert_eq_string(a: string, b: string)` — assert_eq_string: Asserts a == b for strings.
- `pub func assert_eq_bool(a: bool, b: bool)` — assert_eq_bool: Asserts a == b for booleans.

## `text` (7)

- `pub func text_is_blank(s: string) -> bool` — text_is_blank: Returns true if s is empty or whitespace-only. Duplicate of strings::is_blank — kept for compatibility.
- `pub func is_numeric(s: string) -> bool` — is_numeric: Returns true if s parses as an integer.
- `pub func text_count_lines(s: string) -> i32` — text_count_lines: Returns the number of lines in s. Duplicate of strings::count_lines — kept for compatibility.
- `pub func slugify(s: string) -> string` — slugify: Converts s to lowercase with hyphens.
- `pub func indent_text(s: string, n: i32) -> string` — indent_text: Prepends n spaces to each line of s. Duplicate of strings::indent — kept for compatibility.
- `pub func wrap_text(s: string, width: i32) -> List<string>` — wrap_text: Wraps s to width columns, returns list of lines.
- `pub func camel_to_snake(s: string) -> string` — camel_to_snake: Converts CamelCase to snake_case.

## `time` (7)

- `pub func timestamp() -> i64` — timestamp: Current Unix timestamp in seconds.
- `pub func timestamp_ms() -> i64` — timestamp_ms: Current Unix timestamp in milliseconds.
- `pub func sleep_ms(ms: i32)` — sleep_ms: Sleeps for ms milliseconds.
- `pub func elapsed(start: i64) -> i64` — elapsed: Returns ms elapsed since start.
- `pub func seconds_since(start_secs: i64) -> i64` — seconds_since: Returns seconds elapsed since start_secs.
- `pub func millis_since(start_ms: i64) -> i64` — millis_since: Returns ms elapsed since start_ms.
- `pub func duration(start_ms: i64, end_ms: i64) -> i64` — duration: Returns ms between start_ms and end_ms.

