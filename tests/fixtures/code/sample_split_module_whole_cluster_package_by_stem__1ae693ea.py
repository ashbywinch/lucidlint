class User:
    LABEL = 'user'
    def name(self):
        return 'u'

class Team:
    LABEL = 'team'
    def member(self):
        return User().name()
