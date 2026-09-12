def _build_cost_groups(self, data):
    journeys = data.get("journeys", [])
    if not journeys:
        return []
    best = min(journeys, key=lambda j: j.get("duration", 9999))
    mode_single_pence = {}
    current_legs = []

    def _flush_transit():
        nonlocal current_legs
        if not current_legs:
            return
        return [g for g in current_legs]

    return best
