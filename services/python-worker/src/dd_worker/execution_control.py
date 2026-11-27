"""Cooperative cancellation scoped to the single compute thread's execution."""

from contextlib import contextmanager
from contextvars import ContextVar


class ExecutionCancelled(Exception):
    pass


_cancellation = ContextVar("execution_cancellation", default=None)


@contextmanager
def execution_scope(event):
    token = _cancellation.set(event)
    try:
        checkpoint()
        yield
    finally:
        _cancellation.reset(token)


def cancellation_event():
    return _cancellation.get()


def checkpoint():
    event = cancellation_event()
    if event is not None and event.is_set():
        raise ExecutionCancelled()
