"""A module docstring may show code:

fn f() {}
"""

import re

PATTERN = re.compile(r"^def ")
MESSAGE = "def f(): pass"
CODE = "def collect(xs):\n    out = []\n    for x in xs:\n        out.append(x)\n    return out\n"

scan_src("fn f() {}")
helpers.scan_src("fn f() {}")
