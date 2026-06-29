import sys
sys.path.insert(0, '.')

import xmath

print("Python add(2,3) =", xmath.add(2, 3))

p = xmath.Point()
p.x = 10
p.y = 20
print("Python point_sum({10,20}) =", xmath.point_sum(p))

q = xmath.make_point(7, 8)
print("Python make_point(7,8) =", (q.x, q.y))

print("Python greet =", xmath.greet("Mimi"))

print("Python apply_callback =", xmath.apply_callback(lambda a, b: a + b, 5))
