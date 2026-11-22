class Paths:
    @classmethod
    def join(cls, a, b):
        return a + b

    @classmethod
    def split(cls, value):
        return value.split('/')
