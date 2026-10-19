class Base:
    def dump(self, *, by_alias: bool = False) -> dict[str, object]:
        return {"by_alias": by_alias}


class Record(Base):
    def to_dict(self) -> dict[str, object]:
        return self.dump(by_alias=True)
