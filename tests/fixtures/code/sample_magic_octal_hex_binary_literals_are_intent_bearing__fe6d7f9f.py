import os
def f(db, flags, x):
    os.chmod(db, 0o600)
    y = flags & 0xFF
    z = x | 0b1010
    return y or z
