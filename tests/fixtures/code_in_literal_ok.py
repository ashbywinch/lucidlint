"""An example in prose:

def f():
    pass

"""

import re

PATTERN = re.compile(r"^def ")
MESSAGE = "def f(): pass"
FORMAT = "fn {}"
PROSE = "This is prose.\nIt has no code line."
