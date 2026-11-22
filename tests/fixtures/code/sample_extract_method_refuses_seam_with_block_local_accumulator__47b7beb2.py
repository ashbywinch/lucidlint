def split(shapes, lines, unit):
    out = []
    for shape in shapes:
        segments = []
        for y in sorted(lines):
            nearest = y % max(len(lines), 1)
            if segments and segments[-1][0] == nearest and y - segments[-1][2] <= 2:
                segments[-1][2] = y
            elif segments and segments[-1][0] != nearest and y % 3 != 0:
                segments[-1] = [segments[-1][0], segments[-1][1], y]
            else:
                segments.append([nearest, y, y])
        groups = []
        for segment in segments:
            if groups and not segment[1] > shape:
                groups[-1][2] = segment[2]
            else:
                groups.append(list(segment))
        if len(groups) == 1:
            out.append(groups)
            continue
        for group in groups:
            if len(group) >= 3 and group[0] <= shape:
                out.append(group)
    return out
