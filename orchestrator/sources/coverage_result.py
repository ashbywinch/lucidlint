"""One coverage snapshot's covered-line sets and where they came from.

The record also owns the decoding of coverage.py's `line_bits` — the bit set
is this record's wire form.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class CoverageResult:
    """Covered line sets and the source they came from."""

    lines: dict[str, set[int]] | None
    source: str

    @staticmethod
    def numbits_to_lines(numbits: bytes) -> set[int]:
        """Decode coverage.py line_bits: bit (n-1)%8 of byte (n-1)//8 => line n."""
        lines: set[int] = set()
        for byte_idx, byte in enumerate(numbits):
            if byte:
                for bit in range(8):
                    if byte & (1 << bit):
                        lines.add(byte_idx * 8 + bit + 1)
        return lines
