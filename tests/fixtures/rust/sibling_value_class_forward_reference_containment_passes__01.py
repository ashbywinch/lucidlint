from __future__ import annotations

from pathlib import Path


class BatchPaths:
    def __init__(self, work_dir: Path, batch_id: str) -> None:
        self.work_dir = work_dir
        self.batch_id = batch_id

    @property
    def guess_dir(self) -> Path:
        return self.work_dir / self.batch_id / "ocr-guess"

    @property
    def oriented_dir(self) -> Path:
        return self.work_dir / self.batch_id / "oriented"


class BatchReader:
    def __init__(self, paths: "BatchPaths") -> None:
        self.paths = paths
        self.work_dir = paths.work_dir
        self.batch_id = paths.batch_id

    @property
    def guess_dir(self) -> Path:
        return self.work_dir / self.batch_id / "ocr-guess"

    @property
    def oriented_dir(self) -> Path:
        return self.work_dir / self.batch_id / "oriented"
