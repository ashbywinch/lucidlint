async def s_async(people):
    texts = []
    async for p in people:
        texts.extend(p)
    async for q in people:
        texts.extend(q)
    return texts
