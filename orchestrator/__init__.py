"""The orchestrator package — the Python half of lucidlint.

One module per concern, each a class. The package holds no import of libcst
or pygit2 at module level: those arrive through the `Host` the composition
root builds and injects into every collaborator.
"""
