@dataclass
class Inner:
    repo: object
    rel: object

@dataclass
class Outer:
    kind: str
    repo: object
    rel: object
    inner: Inner
