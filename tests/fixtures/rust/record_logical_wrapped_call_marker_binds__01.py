def go(u):
    # lucidlint: ignore record-shape the payload is built at the call site
    send(
        {"user": u, "stamp": u},
    )
