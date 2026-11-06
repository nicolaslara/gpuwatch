#!/bin/sh
# Image ENTRYPOINT: starts gpuwatch beside the container's own command, which
# Modal appends for Functions and Sandboxes alike. Configure it with GPUWATCH_*
# environment variables on the image or the Function; GPUWATCH_DISABLED=1 turns
# it off. Containers without a GPU run the command unchanged.
if [ -e /dev/nvidiactl ] && [ "${GPUWATCH_DISABLED:-0}" != "1" ]; then
  gpuwatch watch &
fi
exec "$@"
