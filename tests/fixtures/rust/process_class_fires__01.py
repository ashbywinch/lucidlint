class RefValidator:
    def validate(self, refs):
        return [r for r in refs if r.ok]

    def reject(self, refs):
        return [r for r in refs if r.bad]
