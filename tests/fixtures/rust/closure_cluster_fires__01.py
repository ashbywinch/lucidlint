class BoxJig:
    def main(self, args):
        width = args.width
        height = args.height

        def scale(w):
            return w * width

        def offset(w):
            return w + width

        def draw(h):
            return h * height

        def fill(h):
            return h - height

        return [scale(w) for w in range(3)], [draw(h) for h in range(3)]
