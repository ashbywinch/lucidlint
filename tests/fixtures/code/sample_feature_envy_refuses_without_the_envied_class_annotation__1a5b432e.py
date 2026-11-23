class _RelationGraph:
    def __init__(self):
        self.em = {}
        self.vm = {}
        self.vs_parent = {}

class _TreeRenderer:
    def __init__(self, graph):
        self.graph = graph

    def render(self):
        graph = self.graph
        em, vm = graph.em, graph.vm
        roots = [vid for vid in vm if vid not in graph.vs_parent]
        x = self.lines
        return roots, x
