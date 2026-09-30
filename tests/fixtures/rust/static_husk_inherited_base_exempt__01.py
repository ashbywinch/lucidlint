class Storage:
    def read(self):
        return self.raw

class GedcomDocument(Storage):
    @staticmethod
    def parse(text):
        return text.splitlines()

    @staticmethod
    def merge(a, b):
        return a + b
