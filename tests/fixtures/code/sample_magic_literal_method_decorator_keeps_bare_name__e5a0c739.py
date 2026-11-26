def tag(value):
    return lambda fn: fn


class C:
    @tag(6)
    def m(self):
        return 1
