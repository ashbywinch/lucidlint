class User:
    def name(self):
        return 'u'

class Team:
    def member(self):
        return User().name()

class Extra:
    def extra(self):
        return 'x'
