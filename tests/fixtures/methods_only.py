class Config:
    def parse(self) -> str:
        return self.raw

    def load(self) -> "Config":
        return self

    def save(self) -> None:
        self.dirty = True
