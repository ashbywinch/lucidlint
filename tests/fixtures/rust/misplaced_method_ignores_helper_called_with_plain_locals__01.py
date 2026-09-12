def format_line(text, width):
    return text[:width]

class R:
    def render(self, text):
        width = 80
        return format_line(text, width)
