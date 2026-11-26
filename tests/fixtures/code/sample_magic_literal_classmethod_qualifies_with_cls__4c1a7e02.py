class Step:
    @classmethod
    def run(cls, max_turns: int) -> int:
        return min(max_turns, 6)
