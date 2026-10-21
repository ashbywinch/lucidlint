class Beta:
    def one(self, value):
        while value:
            value = value - 1
        return value

    def two(self, value):
        with open(value) as fh:
            fh.write("x")
        return value
