def build(writer):
    def add_header():
        writer.lines.append("h")
    def add_footer():
        writer.lines.append("f")
    add_header()
    add_footer()
