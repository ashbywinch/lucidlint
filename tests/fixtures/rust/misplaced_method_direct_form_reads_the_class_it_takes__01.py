class School:
    def __init__(self, name, postcode):
        self.name = name
        self.postcode = postcode


def _format_address(school: School) -> str:
    return f"{school.name}, {school.postcode}"
