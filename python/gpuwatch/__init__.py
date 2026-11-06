"""gpuwatch: read the daemon's state from inside the container."""

import json
import os

STATE_FILE = os.environ.get("GPUWATCH_STATE_FILE", "/tmp/gpuwatch.json")


def state():
    """The daemon's latest state, or None before its first write."""
    try:
        with open(STATE_FILE) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def event():
    """The last throttling event report, or None."""
    s = state()
    return s.get("event") if s else None
