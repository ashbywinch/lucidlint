class GraphContract:
    def __init__(self):
        self.edges = []

def hub_edge_counts(contract: GraphContract):
    return 1

def dominant_callee(contract: GraphContract, x):
    return len(contract.edges) + x

def resolve_callee_module(contract: GraphContract, y):
    return contract.edges
