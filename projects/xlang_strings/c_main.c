#include <stdio.h>
#include <stdint.h>
#include "bindings/xstrings.h"

int main(void) {
    char* msg = greet("Mimi");
    printf("C greet = %s\n", msg);
    mimi_string_free(msg);

    printf("C char_count(\"hello\") = %ld\n", (long)char_count("hello"));

    char* joined = join("Hello, ", "World");
    printf("C join = %s\n", joined);
    mimi_string_free(joined);

    return 0;
}
