class Base:
    def __init__(self):
        self.kind: str = ""

class Sub(Base):
    def refresh(self):
        self.kind = "x"
