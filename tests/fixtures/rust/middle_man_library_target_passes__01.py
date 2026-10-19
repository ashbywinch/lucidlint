from pydantic import BaseModel, ConfigDict


class BoundaryRecord(BaseModel):
    model_config = ConfigDict(populate_by_name=True)

    def to_dict(self) -> dict[str, object]:
        return self.model_dump(by_alias=True)
