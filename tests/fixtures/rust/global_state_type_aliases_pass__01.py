from typing import Callable


class PersonHit:
    name: str


class ItemHit:
    title: str


# a union alias is a type, not a variable a caller varies
ToolArgs = PersonHit | ItemHit
ToolEntry = ToolArgs | dict[str, object]
Answer = tuple[str, int]
Callback = Callable[[int], bool]

# a bare CamelCase target naming a type is an alias too
MyHit = PersonHit
Base = pydantic.BaseModel
Seq = collections.abc.Sequence[int]

# a constant is still a module-level value: the rule's own case, kept reported
MAX_ROWS = 40
