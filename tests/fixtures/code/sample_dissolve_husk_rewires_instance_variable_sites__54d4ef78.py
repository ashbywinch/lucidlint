def fetch_name(user):
    return user.name

class Memory:
    def name(self, user):
        return fetch_name(user)

def use():
    h = Memory()
    return h.name('x')
