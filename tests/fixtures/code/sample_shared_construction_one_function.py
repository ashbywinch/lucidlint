class GeoPoint:
    def __init__(self, lat, lon):
        self.lat = lat
        self.lon = lon


def both(lat, lng):
    first = GeoPoint(float(lat), float(lng))
    return first, GeoPoint(float(lat), float(lng))
