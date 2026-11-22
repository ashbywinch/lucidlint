class C:
    def go(self, xs):
        self.result = []
        for x in xs:
            v = x * 2
            w = v + 1
            if w:
                self.result.append(w)
        return self.result
