def _epic_relations(em):
    return {"a": 1}, {"b": 2}
def _vs_relations(vm, epic_vs, em):
    return 1, 2, 3
class R:
    def __init__(self, em, vm):
        self.em = em
        self.vm = vm
        epic_vs, kids = _epic_relations(em)
        vs_parent, vs_kids, vs_epics = _vs_relations(vm, epic_vs, em)
        self.epic_vs = epic_vs
        self.kids = kids
        self.vs_parent = vs_parent
        self.vs_kids = vs_kids
        self.vs_epics = vs_epics
