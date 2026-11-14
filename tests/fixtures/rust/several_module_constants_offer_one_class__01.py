"""Three module-level constants and the function that reads them."""

HOST = "https://example.test"
TIMEOUT_SECONDS = 30
RETRIES = 3


def fetch():
    return HOST, TIMEOUT_SECONDS, RETRIES
