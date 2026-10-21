import sys
def f():
    try:
        data = parse()
    except ValueError:
        sys.stderr.write('bad')
        sys.exit(2)
    return data
