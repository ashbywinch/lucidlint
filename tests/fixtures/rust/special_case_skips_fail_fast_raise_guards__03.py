def g(mine):
    if mine is None:
        return JSONResponse({'error': 'x'}, status_code=401)
    if mine is None:
        return JSONResponse({'error': 'y'}, status_code=401)
    if mine is None:
        return JSONResponse({'error': 'z'}, status_code=401)
    return mine
