import asyncio


async def poll(check, timeout_s):
    waited = 0
    while waited < timeout_s:
        if check():
            return True
        await asyncio.sleep(1)
        waited += 1
    return False