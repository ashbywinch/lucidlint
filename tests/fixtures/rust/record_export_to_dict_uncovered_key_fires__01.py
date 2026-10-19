from dataclasses import dataclass


@dataclass(frozen=True)
class Report:
    rid: str
    fresh: bool

    def to_dict(self) -> dict:
        return {"rid": self.rid, "fresh": self.fresh, "extra": self.rid}
