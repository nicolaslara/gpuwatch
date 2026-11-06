"""A GPU Function with gpuwatch installed from the wheel: the daemon starts by
itself, and the Function stops taking inputs when an event is recorded, so Modal
replaces the container and its GPU.

    modal run examples/function.py
"""

import time

import modal
import modal.experimental

app = modal.App("gpuwatch-example")
image = modal.Image.debian_slim(python_version="3.12").pip_install("torch==2.9.0", "https://modal-labs-gpuwatch--gpuwatch-web.modal.run/modal_gpuwatch-0.1.0-py3-none-manylinux2014_x86_64.whl")


@app.function(gpu="H100", image=image, timeout=3600)
def train(steps: int = 100) -> dict:
    import gpuwatch
    import torch

    x = torch.randn(8192, 8192, device="cuda", dtype=torch.float16)
    for step in range(steps):
        for _ in range(20):
            x @ x
        torch.cuda.synchronize()
        if event := gpuwatch.event():
            # Finish this input, take no more: Modal starts a fresh container elsewhere.
            modal.experimental.stop_fetching_inputs()
            return {"stopped_at_step": step, "gpu": event["gpu"]["uuid"], "clock_ratio": event["window"]["clock_ratio_busy"]}
        time.sleep(1)
    return {"stopped_at_step": None, "state": gpuwatch.state()}


@app.local_entrypoint()
def main(steps: int = 100):
    print(train.remote(steps))
