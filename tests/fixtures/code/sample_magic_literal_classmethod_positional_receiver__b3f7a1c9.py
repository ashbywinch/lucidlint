class Step:
    @classmethod
    def run(klass, /, max_turns: int) -> int:
        return min(max_turns, 6)
