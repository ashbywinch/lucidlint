class Boundary(BaseModel):
    def to_dict(self) -> dict:
        return self.model_dump(by_alias=True)
