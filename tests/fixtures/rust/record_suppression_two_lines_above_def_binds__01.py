class DeltaFigure:
    value: str
    approx: bool

    # lucidlint: ignore record-shape to_dict construction IS the serialization boundary (coding-standards.md)
    def to_dict(self) -> dict:
        return {"value": self.value, "approx": self.approx}