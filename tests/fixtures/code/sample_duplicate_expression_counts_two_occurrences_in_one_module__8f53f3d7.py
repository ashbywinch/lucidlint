SCALE = 2

def a(one, two):
    first = Rectangle(one.x0 * SCALE, one.y0 * SCALE, one.x1 * SCALE, one.y1 * SCALE)
    second = Rectangle(two.x0 * SCALE, two.y0 * SCALE, two.x1 * SCALE, two.y1 * SCALE)
    return first, second
