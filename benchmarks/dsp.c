#include <stdio.h>

/* Benchmark: first-order low-pass DSP loop (M-014 C baseline, gcc -O2) */
int main(void) {
    const double alpha = 0.01;
    double y = 0.0;
    double x = 0.0;
    for (long long i = 0; i < 50000000LL; i++) {
        x = (double)i * 0.000001;
        y += alpha * (x - y);
    }
    long long out = (long long)(y * 1000000.0);
    printf("%lld\n", out);
    return 0;
}