def run(pages):
    for p in pages:
        t = transcribe(p)
        write(t)
        mark(p)
    t = transcribe(p)
    write(t)
    mark(p)
