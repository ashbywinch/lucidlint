class Marker:
    def __init__(self, signal):
        self.signal = signal


class Suppressions:
    def add(self, marker):
        self.markers.append(marker)


class Grammar:
    def read(self, text):
        return text
