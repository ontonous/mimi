#include <stdint.h>

/* Trusted, deterministic reference functions used by W4 and S1/S4. */
int32_t v02_checksum(int32_t value) { return value * 2; }
int32_t v02_checksum_alt(int32_t value) { return value * 2; }
