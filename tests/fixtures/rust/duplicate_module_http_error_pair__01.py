"""Structured HTTP error carrying status, headers, and Retry-After metadata."""


class HttpError:
    """An HTTP request failed with a non-2xx status code."""

    def __init__(self, status, message="", headers=None):
        self.status = status
        self.headers = headers or {}
        self.message = message

    @property
    def retry_after(self):
        raw = self.headers.get("Retry-After") or self.headers.get("retry-after")
        if raw is None:
            return None
        try:
            return float(raw)
        except (ValueError, TypeError):
            return None

    def is_server_error(self):
        return 500 <= self.status < 600

    def is_rate_limit(self):
        return self.status == 429


def _status_phrase(code):
    return str(code)
