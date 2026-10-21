def span(grid, row, c0, c1):
    lat_min = max(grid.a, grid.a + row * grid.d)
    lat_max = min(grid.b, grid.a + (row + 1) * grid.d)
    lon_min = max(grid.c, grid.c + c0 * grid.e)
    lon_max = min(grid.d, grid.c + (c1 + 1) * grid.e)
    return Rect(lat_min, lat_max, lon_min, lon_max)

class Grid:
    def cell_rect(self, r, c):
        lat_min = max(self.bbox.a, self.bbox.a + r * self.lat_deg)
        lat_max = min(self.bbox.b, self.bbox.a + (r + 1) * self.lat_deg)
        lon_min = max(self.bbox.c, self.bbox.c + c * self.lon_deg)
        lon_max = min(self.bbox.d, self.bbox.c + (c + 1) * self.lon_deg)
        return Rect(lat_min, lat_max, lon_min, lon_max)
