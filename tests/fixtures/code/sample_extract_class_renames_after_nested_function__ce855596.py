class _FnBodyState:
    def __init__(self, line):
        self.line = line

def _score(state: _FnBodyState, i, j):
    def helper(v):
        return v * 2
    if state.line > 0:
        return helper(state.line) + i
    return 0

def _other(state: _FnBodyState, n):
    return state.line + n

def _run(state: _FnBodyState):
    return _score(state, 1, 2)
