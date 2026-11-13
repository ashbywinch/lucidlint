"""A places node's radius, as a plain record."""

PRECISION = 2


class NodeRadius:
    """The radius; the centre point lives on its own record."""

    def __init__(self, value, unit):
        self.value = value
        self.unit = unit
