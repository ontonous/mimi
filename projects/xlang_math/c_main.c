#include <stdio.h>
#include <stdint.h>
#include "bindings/xmath.h"

static int32_t my_callback(int32_t a, int32_t b) {
    return a + b;
}

int main(void) {
    printf("C add(2,3) = %ld\n", (long)add(2, 3));

    struct Point p = { .x = 10, .y = 20 };
    printf("C point_sum({10,20}) = %ld\n", (long)point_sum(p));

    struct Point q = make_point(7, 8);
    printf("C make_point(7,8) = {%d,%d}\n", q.x, q.y);

    char* msg = greet("Mimi");
    printf("C greet = %s\n", msg);
    mimi_string_free(msg);

    printf("C apply_callback(add1, 5) = %ld\n", (long)apply_callback(my_callback, 5));

    return 0;
}
