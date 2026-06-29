#include <stdio.h>
#include <stdint.h>
#include <stdbool.h>
#include "bindings/xcallbacks.h"

static int32_t add_one(int32_t x) {
    return x + 1;
}

static int32_t add(int32_t a, int32_t b) {
    return a + b;
}

static bool is_even(int32_t x) {
    return x % 2 == 0;
}

int main(void) {
    printf("C map_int(add_one, 5) = %ld\n", (long)map_int(add_one, 5));
    printf("C reduce_int(add, 3, 4) = %ld\n", (long)reduce_int(add, 3, 4));
    printf("C filter_int(is_even, 4) = %d\n", (int)filter_int(is_even, 4));
    printf("C filter_int(is_even, 5) = %d\n", (int)filter_int(is_even, 5));
    return 0;
}
