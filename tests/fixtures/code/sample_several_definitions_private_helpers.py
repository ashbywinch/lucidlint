class School:
    """One class and its two private parsers: one concept."""

    def __init__(self, genders, lat, lng):
        self.genders = genders
        self.location = _try_geopoint(lat, lng)

    def accepts(self, requirement, raw):
        return _parse_gender(raw) in self.genders and requirement in self.genders


def _try_geopoint(lat, lng):
    return (float(lat), float(lng))


def _parse_gender(raw):
    return raw.lower()
