def grade_all(rows, mode):
    out = []
    for row in rows:
        s = row['score']
        if s >= 95:
            out.append('A+')
        elif s >= 90:
            out.append('A')
        elif s >= 85:
            out.append('A-')
        elif s >= 80:
            out.append('B+')
        elif s >= 75:
            out.append('B')
        elif s >= 70:
            out.append('B-')
        elif s >= 65:
            out.append('C+')
        elif s >= 60:
            out.append('C')
        elif s >= 55:
            out.append('C-')
        elif s >= 50:
            out.append('D+')
        elif s >= 45:
            out.append('D')
        elif s >= 40:
            out.append('D-')
        else:
            out.append('F')
        if mode == 'debug':
            out.append(row['name'])
    return out
