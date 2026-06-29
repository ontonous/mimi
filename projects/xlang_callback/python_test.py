import sys
sys.path.insert(0, '.')

import xcallbacks

print("Python map_int(add_one, 5) =", xcallbacks.map_int(lambda x: x + 1, 5))
print("Python reduce_int(add, 3, 4) =", xcallbacks.reduce_int(lambda a, b: a + b, 3, 4))
print("Python filter_int(is_even, 4) =", xcallbacks.filter_int(lambda x: x % 2 == 0, 4))
print("Python filter_int(is_even, 5) =", xcallbacks.filter_int(lambda x: x % 2 == 0, 5))
