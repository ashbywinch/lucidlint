class A:
    def go(self, x):
        self.count += 1
        return self.inner.go(x)
