def to_json_value(self):
    result = {}
    try:
        g()
    except Exception:
        logger.exception('x')
        result['value'] = None
    return result
