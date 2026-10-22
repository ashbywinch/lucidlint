class Provenance:
    pass


def load_sources(sources: dict[str, Provenance]) -> None:
    """Build the collection here: the caller's map is being filled in."""
    sources["first"] = Provenance()
