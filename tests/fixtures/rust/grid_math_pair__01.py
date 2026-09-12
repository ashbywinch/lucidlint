class Rect:
    def __init__(self, a, b, c, d):
        self.a = a
        self.b = b
        self.c = c
        self.d = d

def span1(grid, row, c0, c1):
    lat_min = max(grid.a, grid.a + row * grid.d)
    lat_max = min(grid.b, grid.a + (row + 1) * grid.d)
    lon_min = max(grid.c, grid.c + c0 * grid.e)
    lon_max = min(grid.d, grid.c + (c1 + 1) * grid.e)
    return Rect(lat_min, lat_max, lon_min, lon_max)