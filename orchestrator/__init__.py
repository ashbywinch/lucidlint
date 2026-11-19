"""The orchestrator package — the Python half of lucidlint.

One module per concern, each a class; `lucidlint.py` stays the entry point and
re-exports what the tests and the fix command use. The package holds no import
of libcst or pygit2 at module level: those enter through `host`, which reads
the entry module's substitutable bindings (`lucidlint.subprocess`,
`lucidlint._pygit2`, …) at call time.
"""
