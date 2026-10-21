class DeltaFigure:
    value: str
    approx: bool

    # lucidlint: ignore record-shape the snapshot IS the wire contract the reader cannot type (coding-standards.md)
    def snapshot(self) -> dict:
        return {}
