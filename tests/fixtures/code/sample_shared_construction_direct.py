class GeoPoint:
    def __init__(self, lat, lon):
        self.lat = lat
        self.lon = lon


def from_row(lat: float, lng: float):
    return GeoPoint(lat, lng)
