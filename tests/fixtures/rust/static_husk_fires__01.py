class GedcomDocument:
    @staticmethod
    def parse(text):
        return text.splitlines()

    @staticmethod
    def merge(a, b):
        return a + b
