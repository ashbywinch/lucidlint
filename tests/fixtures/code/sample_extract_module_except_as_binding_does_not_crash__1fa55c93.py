def tokenize(s):
    try:
        return s.split()
    except ValueError as e:
        return str(e)

def words(s):
    return tokenize(s)
