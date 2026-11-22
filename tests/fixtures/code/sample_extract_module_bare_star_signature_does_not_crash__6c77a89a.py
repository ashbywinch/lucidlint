def tokenize(s, *, limit=None):
    return s.split()[:limit]

def words(s):
    return tokenize(s)
