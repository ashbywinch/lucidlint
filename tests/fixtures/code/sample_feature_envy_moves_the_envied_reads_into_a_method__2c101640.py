class _RelationGraph:
    def __init__(self):
        self.em = {}
        self.vm = {}
        self.vs_parent = {}
        self.parent_epic = {}

class _TreeRenderer:
    def __init__(self, graph: _RelationGraph):
        self.graph: _RelationGraph = graph
        self.lines = []

    def render(self):
        graph = self.graph
        em, vm = graph.em, graph.vm
        roots = [vid for vid in vm if vid not in graph.vs_parent]
        epic_roots = [cid for cid in em if cid not in graph.parent_epic]
        x = self.lines
        return roots, epic_roots, x
