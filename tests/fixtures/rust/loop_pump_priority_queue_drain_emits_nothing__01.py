import heapq


async def drain(pq):
    out = []
    while True:
        item = heapq.heappop(pq)
        out.append(item)
        heapq.heappush(pq, item.next)
        await handle(item)
    return out