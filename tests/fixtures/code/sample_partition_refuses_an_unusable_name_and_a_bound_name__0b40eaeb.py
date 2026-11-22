class Report:
    def __init__(self):
        self.rows = []
        self.total = 0
        self.title = ""
        self.author = ""

    def add_row(self) -> None:
        self.rows.append(1)

    def row_count(self) -> int:
        return len(self.rows) + self.total

    def clear_rows(self) -> None:
        self.rows = []
        self.total = 0

    def set_title(self) -> None:
        self.title = "x"

    def set_author(self) -> None:
        self.author = "y"

    def describe(self) -> str:
        return self.title + self.author
