
from pathlib import Path


def text_path(work: Path, batch: str, page: str) -> Path:
    return work / batch / "ocr-guess" / f"{page}.txt"


def marker_path(work: Path, batch: str, page: str) -> Path:
    return work / batch / "ocr-guess" / f"{page}.rows.json"
