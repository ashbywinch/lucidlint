def f(x):
    client.post(url, headers={"Content-Type": "json", "X": x})
    table = {"a": 1, "b": 2}
    return table
