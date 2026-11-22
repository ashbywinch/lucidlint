
from pathlib import Path


def guess_dir(work: Path, batch: str) -> Path:
    return work / batch / "ocr-guess"


def oriented_dir(work: Path, batch: str) -> Path:
    return work / batch / "oriented"
