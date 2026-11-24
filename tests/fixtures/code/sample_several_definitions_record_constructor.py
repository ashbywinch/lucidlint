class Marker:
    def __init__(self, signal):
        self.signal = signal


class Suppressions:
    def add(self, marker):
        self.markers.append(marker)


def parse(text):
    return text
