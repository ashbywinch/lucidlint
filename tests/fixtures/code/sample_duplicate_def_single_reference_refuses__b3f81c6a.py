def wrap_field(code, tree):
    return code[tree]


def wrap_list(code, tree):
    return code[tree]


def driver(code, tree):
    return wrap_field(code, tree) + wrap_list(code, tree)
