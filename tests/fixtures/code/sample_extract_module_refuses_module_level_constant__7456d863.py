LIMIT = 10

def tokenize(s):
    return s.split()[:LIMIT]

def words(s):
    return tokenize(s)
