from geo import GeoPoint


def from_json(data):
    return GeoPoint(float(data["lat"]), float(data["lon"]))
