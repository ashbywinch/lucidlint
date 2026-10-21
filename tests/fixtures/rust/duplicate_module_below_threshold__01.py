class Alpha:
    def one(self, value):
        if value:
            return value
        return None

    def two(self, value):
        for item in value:
            print(item)
        return value
