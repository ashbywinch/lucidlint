"""setup.py — build the Rust scan core into the wheel.

`pip install .` (or `pip wheel`) builds the scanner crate's release binary
and ships it as opaque package-data at ``lucidlint_bin/bin/lucidlint`` inside
the installed package. The orchestrator's ``_scanner_candidates`` finds it as
a sibling of the module, so a pip install is self-contained: scan, fix, and
LSP all work with no release bundle and no PATH.

The binary is platform-specific, so the wheel is forced to a platform tag
(``py3-none-<plat>``) — never the misleading ``py3-none-any`` a data-only
wheel would otherwise get.
"""

import os
import shutil
import subprocess
import sysconfig
from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py
from wheel.bdist_wheel import bdist_wheel

HERE = Path(__file__).parent
EXE = ".exe" if os.name == "nt" else ""
BIN_SOURCE = HERE / "scanner" / "target" / "release" / f"lucidlint{EXE}"
BIN_DEST = HERE / "lucidlint_bin" / "bin" / f"lucidlint{EXE}"


class PlatformWheel(bdist_wheel):
    """Our wheel carries a native binary as package-data, so it must carry a
    platform tag — be honest in the metadata, never ship a pure-py wheel."""

    # lucidlint: ignore detached-method setuptools invokes these hooks as bound methods
    def get_tag(self):
        # override the default "py3-none-any": our opaque package-data binary
        # would otherwise produce a wrongly tagged pure-py wheel. The binary is
        # python-version independent but platform-specific, so compute the real
        # platform tag (linux_x86_64, macosx_*, win_amd64).
        plat = sysconfig.get_platform().replace("-", "_").replace(".", "_")
        return ("py3", "none", plat)


class BuildPy(build_py):
    def run(self) -> None:
        subprocess.run(
            ["cargo", "build", "--release", "--manifest-path", "scanner/Cargo.toml"],
            cwd=HERE,
            check=True,
        )
        BIN_DEST.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(BIN_SOURCE, BIN_DEST)
        super().run()


# lucidlint: ignore record-shape cmdclass is setuptools' plugin wire —
# a name->class registry, not a domain record
setup(cmdclass={"build_py": BuildPy, "bdist_wheel": PlatformWheel})
