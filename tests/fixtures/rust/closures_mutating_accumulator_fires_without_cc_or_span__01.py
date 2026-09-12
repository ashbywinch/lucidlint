def render(em, vm):
    L = []
    def emit_epic(cid, d):
        L.append(cid)
        for c in em:
            emit_epic(c, d + 1)
    def emit_vs(vid, d=0):
        L.append(vid)
        for c in vm:
            emit_vs(c, d + 1)
    return L
