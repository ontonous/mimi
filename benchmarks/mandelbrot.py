"""Benchmark: Mandelbrot set computation"""

def mandelbrot_iterations(cx, cy):
    zx = zy = 0.0
    for i in range(1000):
        zx2 = zx * zx
        zy2 = zy * zy
        if zx2 + zy2 > 4.0:
            return i
        zy = 2.0 * zx * zy + cy
        zx = zx2 - zy2 + cx
    return 1000

total = 0
for y in range(100):
    cy = y / 50.0 - 1.0
    for x in range(100):
        cx = x / 50.0 - 1.5
        total += mandelbrot_iterations(cx, cy)
print(total)
