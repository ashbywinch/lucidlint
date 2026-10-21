from dataclasses import dataclass


class Codec:
    def to_dict(self, a: int, b: str) -> dict:
        return {"a": a, "b": b}


@dataclass(frozen=True)
class Report:
    rid: str
    fresh: bool

    def snapshot(self) -> dict:
        return {"rid": self.rid, "fresh": self.fresh}


def to_dict(a: int, b: str) -> dict:
    return {"a": a, "b": b}
