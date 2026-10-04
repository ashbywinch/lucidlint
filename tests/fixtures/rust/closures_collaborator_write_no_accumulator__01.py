def apply_patches(rid, body):
    prop = get_property(rid)

    def _names(value):
        if not isinstance(value, list):
            raise ValueError("bad")
        return value

    payers = _names(body["payers"])

    def _apply():
        if payers is not None:
            prop.payers.push(payers, "user")

    _apply()
    return {"status": "ok"}
