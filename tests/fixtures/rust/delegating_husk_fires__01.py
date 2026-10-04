def fetch_name(user):
    return user.name


def fetch_age(user):
    return user.age


class Memory:
    def name(self, user):
        return fetch_name(user)

    def age(self, user):
        return fetch_age(user)
