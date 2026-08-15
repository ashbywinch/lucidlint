def parse_config(config: Config) -> str:
    return config.raw


def load_config(config: Config) -> Config:
    return config


def save_config(config: Config) -> None:
    config.dirty = True
