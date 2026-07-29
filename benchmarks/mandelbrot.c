/* Benchmark: Mandelbrot set computation */
#include <stdio.h>

int mandelbrot_iterations(double cx, double cy) {
    double zx = 0.0, zy = 0.0;
    for (int i = 0; i < 1000; i++) {
        double zx2 = zx * zx;
        double zy2 = zy * zy;
        if (zx2 + zy2 > 4.0) return i;
        zy = 2.0 * zx * zy + cy;
        zx = zx2 - zy2 + cx;
    }
    return 1000;
}

int main() {
    int total = 0;
    for (int y = 0; y < 100; y++) {
        double cy = y / 50.0 - 1.0;
        for (int x = 0; x < 100; x++) {
            double cx = x / 50.0 - 1.5;
            total += mandelbrot_iterations(cx, cy);
        }
    }
    printf("%d\n", total);
    return 0;
}
