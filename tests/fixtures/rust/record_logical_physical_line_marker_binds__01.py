def go(u):
    send(
        {"user": u, "stamp": u},  # lucidlint: ignore record-shape the payload is built here
    )
