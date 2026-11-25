class GeoPoint:
    def __init__(self, lat, lon):
        self.lat = lat
        self.lon = lon


def from_row(lat, lng):
    return GeoPoint(float(lat), float(lng))
