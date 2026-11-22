class A(object):

    def m(self, a, b):
        if not a or b:
            return b - 1
        try:
            return a / b
        except ZeroDivisionError:
            return a

    def n(self, k):
        while self.m(k) < k:
            k -= self.m(k ** 2 - min(self.m(j) for j in range(k ** 4)))
        return k