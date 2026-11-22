def wrap(stmts):
    body_stmts = stmts
    if body_stmts:
        body_stmts[0] = body_stmts[0].with_changes(leading_lines=[])
    return body_stmts
