from dataclasses import dataclass


@dataclass(frozen=True)
class Report:
    """A record whose class names the shape."""

    rid: str
    fresh: bool

    def to_dict(self) -> dict:
        return dict(rid=self.rid, fresh=self.fresh)

    def to_json(self) -> dict[str, object]:
        return {"rid": self.rid, "fresh": self.fresh}


class Session:
    def __init__(self, uid: str, fresh: bool) -> None:
        self.uid = uid
        self.fresh = fresh

    def to_dict(self):
        return {"uid": self.uid, "fresh": self.fresh}
