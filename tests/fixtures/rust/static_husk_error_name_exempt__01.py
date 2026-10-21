class ParseError(Exception):
    @staticmethod
    def describe(code):
        return f"err {code}"

    @staticmethod
    def wrap(cause):
        return cause
