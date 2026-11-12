"""Backoff helpers."""
import time


def retry_with_backoff(fn, tries=3):
    """Retry a call, doubling the wait."""
    delay = 1
    for attempt in range(tries):
        try:
            return fn()
        except ValueError:
            time.sleep(delay)
            delay *= 2
    raise RuntimeError("gave up")


class Backoff:
    def __init__(self, base=2):
        self.base = base

    def wait(self, attempt):
        return self.base ** attempt
