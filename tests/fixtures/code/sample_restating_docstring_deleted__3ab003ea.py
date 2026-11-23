def check(box, line):
    """the line orientation must be consistent with the box aspect"""
    orientation = line.orientation
    consistent = box.aspect
    return orientation == consistent
