def api_fetch(client):
    def _fetch(request):
        a = client.get(request)
        b = a.status_code
        if b >= 400:
            raise RuntimeError(a.text)
        c = a.json()
        d = c.get("value", 0)
        e = int(d)
        return e

    return _fetch


def other_fetch(client):
    a = client.post("/other")
    b = a.status_code
    if b >= 400:
        raise RuntimeError(a.text)
    c = a.json()
    d = c.get("value", 0)
    e = int(d)
    return e
