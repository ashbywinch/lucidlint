class R:
    def render(self):
        graph = self.graph
        em, vm = graph.em, graph.vm
        roots = [v for v in vm if v not in graph.vs_parent]
        done = [v for v in vm if v not in graph.kids]
        return roots, em, done
