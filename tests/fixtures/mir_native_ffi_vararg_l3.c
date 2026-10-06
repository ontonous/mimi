#include <stdarg.h>
#include <stdint.h>

int32_t mimi_ffi_vararg_probe(int32_t tag, ...) {
    va_list arguments;
    va_start(arguments, tag);
    int32_t result = -100;
    switch (tag) {
    case 0:
        result = 16;
        break;
    case 1: {
        int value = va_arg(arguments, int);
        result = value == 1 ? 11 : -11;
        break;
    }
    case 2: {
        double value = va_arg(arguments, double);
        result = value == 1.5 ? 12 : -12;
        break;
    }
    case 3: {
        int flag = va_arg(arguments, int);
        double value = va_arg(arguments, double);
        result = flag == 0 && value == 1.25 ? 13 : -13;
        break;
    }
    case 4: {
        int first = va_arg(arguments, int);
        int64_t second = va_arg(arguments, int64_t);
        result = first == -7 && second == INT64_C(9000000000) ? 14 : -14;
        break;
    }
    case 5: {
        double value = va_arg(arguments, double);
        result = value == 2.5 ? 15 : -15;
        break;
    }
    default:
        break;
    }
    va_end(arguments);
    return result;
}
