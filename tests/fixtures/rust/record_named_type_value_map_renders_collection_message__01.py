from decimal import Decimal


class Provenance:
    pass


def convert(data: dict[str, Decimal]) -> dict[str, Decimal]:
    return data


def index(rows: dict[str, list]) -> None:
    pass


def build():
    return {
        "inbox": Provenance,
        "archive": Provenance,
    }