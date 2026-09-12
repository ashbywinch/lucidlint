class C:
    kind: str = "x"
    def __init__(self):
        self.x: int = 0
    def leave(self):
        self.done = True
        self.kind = "y"
