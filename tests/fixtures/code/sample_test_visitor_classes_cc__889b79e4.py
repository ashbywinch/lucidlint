class B(object):

    ATTR = 1 if A().n(1) == 1 else 2
    import sys
    if sys.version_info >= (3, 3):
        import os
        AT = os.open('/random/loc')

    def __iter__(self):
        return __import__('itertools').tee(B.__dict__)

    def test(self, func):
        a = func(self.ATTR, self.AT)
        if a < self.ATTR:
            yield self
        elif a > self.ATTR ** 2:
            yield self.__iter__()
        yield iter(a)