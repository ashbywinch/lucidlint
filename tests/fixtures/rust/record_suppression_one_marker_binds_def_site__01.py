# lucidlint: ignore record-shape keyed collection, not a record — naptan is a stop-name→coordinate lookup table over
def _merge_dataset_results(
    datasets: list[dict],
    stations: list,
    api_key: str,
    cached_only: bool,
    naptan: dict[str, tuple[float, float]] | None,
) -> None:
    return None