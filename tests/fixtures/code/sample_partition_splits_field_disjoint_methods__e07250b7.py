class Account:
    def __init__(self, balance: int, limit: int, name: str, title: str):
        self.balance = balance
        self.limit = limit
        self.name = name
        self.title = title

    def deposit(self, amount: int) -> int:
        self.balance += min(amount, self.limit)
        return self.balance

    def withdraw(self, amount: int) -> int:
        self.balance -= min(amount, self.limit)
        return self.balance

    def fee(self) -> int:
        return self.balance // self.limit

    def label(self) -> str:
        return self.name.upper() + self.title

    def greet(self) -> str:
        return 'Hi ' + self.name + ' ' + self.title

    def rename(self, name: str) -> None:
        self.name = name
