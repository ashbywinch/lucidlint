async def fetch_all(urls):
    results = []
    async for u in urls:
        results.append(await fetch(u))
    return results


async def poll(client):
    out = []
    while True:
        out.append(await client.read())
    return out
