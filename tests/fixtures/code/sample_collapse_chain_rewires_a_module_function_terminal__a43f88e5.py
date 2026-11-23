def format_currency(value):
    return f'{value:.2f}'

def show_price(value):
    return format_currency(value)

class Panel:
    def display(self, value):
        return show_price(value)
