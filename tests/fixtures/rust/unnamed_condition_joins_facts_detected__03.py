class Account:
    def __eq__(self, other):
        return self.owner == other.owner and self.balance == other.balance and self.kind == other.kind
