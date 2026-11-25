class School:
    def __init__(self, name, postcode):
        self.name = name
        self.postcode = postcode


def _school_name(school: School) -> str:
    return school.name


def _identity(school: School) -> School:
    return school


def _describe(school: School) -> str:
    name = school.name
    postcode = school.postcode
    return f"{name} {postcode}"


def _label(school: School) -> str:
    return school.name if school.postcode else "N/A"


def _validate(school: School, rules: dict) -> bool:
    return school.name in rules and school.postcode in rules


def _parse(raw: str) -> str:
    return raw
