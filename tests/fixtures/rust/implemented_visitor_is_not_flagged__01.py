class A:
    def accept(self, v):
        return v.visit_a(self)

class B:
    def accept(self, v):
        return v.visit_b(self)

class Visitor:
    def visit_a(self, e):
        return 1
    def visit_b(self, e):
        return 2
