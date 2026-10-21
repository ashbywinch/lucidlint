from abc import ABC, abstractmethod

class Parser(ABC):
    @staticmethod
    def split(text):
        return text.splitlines()

    @abstractmethod
    def parse(self, text):
        pass