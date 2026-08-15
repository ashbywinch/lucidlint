def parse_config(config: Config) -> str:
    return config.raw


def load_record(record: Record) -> Record:
    return record


def save_verdict(verdict: Verdict) -> None:
    verdict.saved = True
