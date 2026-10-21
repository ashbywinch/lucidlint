def check(box, line):
    """admission gate: the reading order must stay on the ink axis"""
    orient = line.orientation
    aspect = box.aspect
    return orient == aspect
