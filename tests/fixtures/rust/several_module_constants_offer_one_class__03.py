"""Three constants among other module-level bindings the rule treats
differently: a mutable container, a re-binding, a call-valued service and a
dunder protocol name. Only the constants count toward the class nudge."""

HOST = "https://example.test"
TIMEOUT_SECONDS = 30
RETRIES = 3
CACHE = {}
ALIAS = other.VALUE
CLIENT = Client()
__all__ = ["HOST"]


def fetch():
    return HOST, TIMEOUT_SECONDS, RETRIES, CACHE, ALIAS, CLIENT
