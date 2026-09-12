import os
def f(cfg):
    a = cfg.get('retryable', False)
    b = getattr(cfg, 'strict', True)
    c = os.environ.get('DEBUG', False)
    d = cfg.setdefault('cached', False)
    e = cfg.pop('stale', True)
    return a, b, c, d, e
