def _superseded_lines(em, vm):
    return len(em) + len(vm)

class R:
    def __init__(self, em, vm):
        self.em = em
        self.vm = vm
    def render(self):
        em, vm = self.em, self.vm
        return _superseded_lines(em, vm)
