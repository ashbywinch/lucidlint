class Mark:
    def width(self):
        return self.w


class Ink:
    def size(self):
        return self.n


def draw(mark):
    return mark.width()


def erase(ink):
    return ink.size()
