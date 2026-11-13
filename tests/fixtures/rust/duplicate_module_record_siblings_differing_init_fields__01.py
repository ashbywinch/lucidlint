"""A places node's centre, as a plain record."""

PRECISION = 2


class NodeCenter:
    """The centre point; the radius lives on its own record."""

    def __init__(self, lat, lon):
        self.lat = lat
        self.lon = lon
