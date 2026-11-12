"""A multi-node training job with gpuwatch: every 50 steps the ranks agree on
whether any GPU is throttling, save a checkpoint and fail together, and the
retry starts a new cluster that resumes from the checkpoint.

    modal run examples/clustered.py
"""

import modal
import modal.experimental

app = modal.App("gpuwatch-clustered-example")
image = modal.Image.debian_slim(python_version="3.12").pip_install("torch==2.9.0", "https://github.com/nicolaslara/gpuwatch/releases/latest/download/modal_gpuwatch-0.1.0-py3-none-manylinux2014_x86_64.whl")
volume = modal.Volume.from_name("gpuwatch-clustered-example", create_if_missing=True)
CHECKPOINT = "/ckpt/step.txt"


def gpu_throttled() -> bool:
    import gpuwatch
    import torch
    import torch.distributed as dist

    # All ranks must agree, or the healthy ones hang in the next collective.
    flag = torch.tensor([1 if gpuwatch.event() else 0], device="cuda")
    dist.all_reduce(flag, op=dist.ReduceOp.MAX)
    return bool(flag.item())


@app.function(gpu="H100", image=image, volumes={"/ckpt": volume}, timeout=3600, retries=modal.Retries(max_retries=3, initial_delay=0.0))
@modal.experimental.clustered(size=2)
def train(steps: int = 1000):
    import os

    import torch
    import torch.distributed as dist

    info = modal.experimental.get_cluster_info()
    dist.init_process_group("nccl", init_method=f"tcp://{info.container_ips[0]}:29500", rank=info.rank, world_size=len(info.container_ips))
    volume.reload()
    start = int(open(CHECKPOINT).read()) if os.path.exists(CHECKPOINT) else 0

    x = torch.randn(8192, 8192, device="cuda", dtype=torch.float16)
    for step in range(start, steps):
        for _ in range(20):
            x @ x
        dist.barrier()
        if step % 50 == 0 and gpu_throttled():
            if info.rank == 0:
                with open(CHECKPOINT, "w") as f:
                    f.write(str(step))
                volume.commit()
            dist.barrier()
            raise RuntimeError(f"GPU throttling at step {step}, retrying on a new cluster")


@app.local_entrypoint()
def main(steps: int = 1000):
    train.remote(steps)
