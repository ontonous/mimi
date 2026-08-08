# Benchmark: first-order low-pass DSP loop (CPython baseline)
alpha = 0.01
y = 0.0
x = 0.0
for i in range(50_000_000):
    x = i * 0.000001
    y = y + alpha * (x - y)
print(int(y * 1_000_000.0))