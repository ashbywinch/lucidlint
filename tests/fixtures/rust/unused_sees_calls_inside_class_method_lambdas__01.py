def helper(x):
    return x + 1

class C:
    def m(self, xs):
        return max(xs, key=lambda v: helper(v))
