class Report:
    def __init__(self):
        self.rows = []
        self.total = 0
        self.title = ""
        self.author = ""

    def add_row(self, row):
        self.rows.append(row)

    def row_count(self):
        return len(self.rows)

    def clear_rows(self):
        self.rows = []
        self.total = 0

    def set_title(self, title):
        self.title = title

    def set_author(self, author):
        self.author = author

    def describe(self):
        return self.title + self.author

    def summary(self):
        return self.title + str(self.total)
