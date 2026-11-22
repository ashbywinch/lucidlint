class User:
    def name(self):
        return Team.NAME

class Team:
    NAME = 't'
    def member(self):
        return User().name()
