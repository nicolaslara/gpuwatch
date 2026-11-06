"""A GPU Sandbox from an image with gpuwatch: the daemon starts with any Python
command in the Sandbox, the controller reads the state over exec and replaces
the Sandbox on an event.

    modal run examples/sandbox.py
"""

import json
import time

import modal

LOAD = "import sys,time,torch;x=torch.randn(8192,8192,device='cuda',dtype=torch.float16);t=time.time()+int(sys.argv[1])\nwhile time.time()<t: x@x"

app = modal.App("gpuwatch-sandbox-example")
image = modal.Image.debian_slim(python_version="3.12").pip_install("torch==2.9.0", "https://github.com/modal-projects/gpuwatch/releases/latest/download/modal_gpuwatch-0.1.0-py3-none-manylinux2014_x86_64.whl")


def event(sb: modal.Sandbox):
    """The daemon's last event, or None; `gpuwatch event` exits 2 when there is one."""
    p = sb.exec("gpuwatch", "event")
    return json.loads(p.stdout.read()) if p.wait() == 2 else None


@app.local_entrypoint()
def main(seconds: int = 60):
    sb = modal.Sandbox.create(image=image, gpu="H100", app=app, timeout=600)
    sb.exec("python", "-c", LOAD, str(seconds))
    time.sleep(seconds + 5)
    if e := event(sb):
        print(f"throttling on {e['gpu']['uuid']}, clock at {e['window']['clock_ratio_busy']:.0%} of max: replacing the sandbox")
        sb.terminate()
        sb = modal.Sandbox.create(image=image, gpu="H100", app=app, timeout=600)
    sb.terminate()
