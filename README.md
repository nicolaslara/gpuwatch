# gpuwatch

Tells you when the GPU under your Modal container is thermally throttling and by how much, in your container logs and in a state file your code can act on. One small daemon per container, started automatically; a poll costs about 20 µs per GPU every 2 seconds.

## Install

One line on your image:

```python
image = modal.Image.debian_slim().pip_install(
    "https://github.com/nicolaslara/gpuwatch/releases/latest/download/modal_gpuwatch-0.1.0-py3-none-manylinux2014_x86_64.whl"
)
```

Without pip, `.run_commands("curl -fsSL https://github.com/nicolaslara/gpuwatch/releases/latest/download/install.sh | sh")` installs the same files for the image's Python (the script fetches with curl, wget or Python, whichever exists). The wheel may be published to PyPI as `modal-gpuwatch` later.

Every Function or Sandbox container with a GPU then starts `gpuwatch watch` as soon as Python starts; nothing in your code changes. For images whose command is not Python, add `.dockerfile_commands('ENTRYPOINT ["gpuwatch-entrypoint"]')` after the install line; an image with its own entrypoint chains it, `ENTRYPOINT ["gpuwatch-entrypoint", "/your/entrypoint"]`, and keeps its arguments. Linux x86_64, glibc 2.17 or newer (CentOS 7, Ubuntu 18.04, Debian 10 and anything later); Alpine is not supported.

## What you see

A throttled GPU puts one line in your container logs, once per window. The daemon prints nothing else unless `GPUWATCH_VERBOSE=1` asks for a startup line:

```
gpuwatch: THROTTLING GPU 0 NVIDIA H100 80GB HBM3 (GPU-54655b4f-…): 10.0 busy min, thermal 100% of samples, clock 32% of max (32% while throttled), temp max 91 C, power 0.31 of limit
```

The same event, as JSON, goes to the state file. Nothing leaves the container; Modal's own fleet monitoring logs the same GPUs on its side.

## What counts as throttling

Only busy samples count (GPU utilization above 50%). A window of busy minutes is an event when at least a fraction of its samples carry a thermal slowdown reason from the driver and the SM clock on those samples averages at least a given reduction below the device maximum.

| `GPUWATCH_SENSITIVITY` | window | thermal samples | clock reduction | Meaning |
|---|---|---|---|---|
| `low` | 10 min | 50% | 50% | half speed or worse |
| `medium` (default) | 10 min | 50% | 30% | the rule Modal's own fleet monitoring uses |
| `high` | 5 min | 30% | 15% | a 15% loss, beyond what a warm healthy GPU shows |

A warm healthy GPU runs at 90% or more of its rated clock and never fires. Power capping is not thermal and never fires either: an L4 under a dense matmul sits at 45% of its rated clock with only the power-cap reason set, and that is normal.

## Reacting

The daemon writes `/tmp/gpuwatch.json` every 30 seconds and on every event. In a Function, check it between inputs and stop taking new ones, so Modal replaces the container (`examples/function.py`):

```python
import gpuwatch, modal.experimental

if gpuwatch.event():
    modal.experimental.stop_fetching_inputs()
```

For a multi-node job, have every rank check on the same step, save a checkpoint and raise, so a retry resumes on a new cluster (`examples/clustered.py`).

For a Sandbox, `exec("gpuwatch", "event")` prints the last event and exits 2 when there is one; recreate the Sandbox on it (`examples/sandbox.py`). `GPUWATCH_ON_EVENT` runs a shell command once per event, with the report in `GPUWATCH_REPORT`.

## Configuration

Environment variables on the image (`.env`) or the Function: `GPUWATCH_SENSITIVITY`, `GPUWATCH_WINDOW` (busy minutes), `GPUWATCH_REDUCTION`, `GPUWATCH_INTERVAL` (seconds), `GPUWATCH_STATE_FILE`, `GPUWATCH_ON_EVENT`, `GPUWATCH_VERBOSE=1` (startup line), `GPUWATCH_QUIET=1` (errors and events only), `GPUWATCH_DISABLED=1`. The same settings are flags of `gpuwatch watch`; in a shell, `gpuwatch check --seconds 30` gives a one-shot verdict and `gpuwatch event` prints the daemon's last event (both exit 2 when throttling).

## Building

`./build.sh` builds the binary in a manylinux2014 container and the wheel (needs Docker); `cargo test` runs the rule tests. Tags `v*` publish both through GitHub Actions.
