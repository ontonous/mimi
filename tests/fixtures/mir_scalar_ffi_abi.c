#include <stdint.h>
#include <stdbool.h>
int32_t mir_ffi_i32(int32_t x) { return x; }
int64_t mir_ffi_i64(int64_t x) { return x; }
bool mir_ffi_bool(bool x) { return !x; }
double mir_ffi_f64(double x) { return x; }
int64_t mir_ffi_f64_code(double x) { return x == 42.5 ? 1 : 0; }
static int64_t sequence = 0;
void mir_ffi_store(int32_t x) { sequence = sequence * 10 + x; }
int64_t mir_ffi_read(void) { return sequence; }
