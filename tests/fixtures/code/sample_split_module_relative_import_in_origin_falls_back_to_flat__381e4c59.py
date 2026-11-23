from .common import fmt

class User:
    def name(self):
        return 'u'

class Team:
    def member(self):
        return fmt(User().name())
