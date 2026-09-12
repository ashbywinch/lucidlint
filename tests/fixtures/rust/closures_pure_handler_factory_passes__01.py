def make_handlers(config, db):
    def on_get(request):
        return db.query(config.filter)
    def on_post(request):
        return db.read(request)
    return on_get, on_post
