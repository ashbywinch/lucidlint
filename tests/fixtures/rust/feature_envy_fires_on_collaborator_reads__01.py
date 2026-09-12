class R:
    def render(self):
        graph = self.graph
        a = graph.vs_parent
        b = graph.kids
        c = graph.epic_vs
        d = graph.vs_kids
        e = graph.vs_epics
        f = graph.epic_vs
        g = graph.vs_kids
        x = self.lines
        y = self.count
        return a, b, c, d, e, f, g, x, y
