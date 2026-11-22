class _FnBodyState:
    def __init__(self, line):
        self.line = line

def _window_score(state: _FnBodyState, i, j, min_lines):
    if state.line > 0:
        return 1
    return 0

def _window_has_outvars(state: _FnBodyState, j, writes_all):
    return state.line > 0

def _best_seam(state: _FnBodyState, min_lines=2):
    return _window_score(state, 0, 1, min_lines)
