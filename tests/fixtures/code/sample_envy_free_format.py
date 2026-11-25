from school import School


def address(school: School) -> str:
    return f"{school.name}, {school.postcode}, {school.town}, {school.region}"
