SCALE = 2

def b(items):
    return tuple([Rectangle(item.x0 * SCALE, item.y0 * SCALE, item.x1 * SCALE, item.y1 * SCALE) for item in items if item.visible])
