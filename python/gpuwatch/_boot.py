"""Starts the daemon when a Python interpreter starts in a Modal GPU container.

Imported by gpuwatch_boot.pth at interpreter startup. Cheap and silent when
there is nothing to do; the daemon itself exits at once when one already runs.
"""

import os
import subprocess
import sys


def start():
    if os.environ.get("GPUWATCH_DISABLED", "0") == "1" or "MODAL_TASK_ID" not in os.environ:
        return
    if not os.path.exists("/dev/nvidiactl") or sys.flags.no_site:
        return
    binary = os.path.join(os.path.dirname(__file__), "bin", "gpuwatch")
    if not os.access(binary, os.X_OK):
        binary = "gpuwatch"
    try:
        subprocess.Popen([binary, "watch"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, start_new_session=True, close_fds=True)
    except OSError:
        pass


start()
