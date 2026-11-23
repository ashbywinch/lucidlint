class Pair:
    def __init__(self, a, b):
        self.a = a

def merge(x, y, pair):
    return x + y + pair.a

def build():
    return merge('a', 'b', Pair(1, 2))
