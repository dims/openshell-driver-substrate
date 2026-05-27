#!/usr/bin/env bash
# Pre-substrate validation: drives docker --runtime=runsc-gpu directly to
# stress-test the cuda-checkpoint wrapper on any L40S/H100/A100 host.
# Prereqs (see README.md): runsc, cuda-checkpoint, the wrapper, the
# runsc-gpu docker runtime, the persistenced-socket workaround.
set -euo pipefail

CTR=gpu-counter-bare
IMAGE="${IMAGE:-nvidia/cuda:12.6.3-runtime-ubuntu24.04}"
CP=/tmp/gpu-counter-cp

trap 'docker rm -f "$CTR" 2>/dev/null || true; sudo rm -rf "$CP"' EXIT
docker rm -f "$CTR" 2>/dev/null || true

docker run -d --name "$CTR" --runtime=runsc-gpu --gpus all \
  -v /usr/local/bin/cuda-checkpoint:/usr/local/bin/cuda-checkpoint:ro \
  -v /usr/local/bin/cuda-checkpoint-wrapper.sh:/usr/local/bin/cuda-checkpoint-wrapper.sh:ro \
  "$IMAGE" \
  bash -c 'apt-get update -qq >/dev/null && apt-get install -y -qq python3-minimal >/dev/null && python3 -uc "
import ctypes, time
libcuda = ctypes.CDLL(\"libcuda.so.1\")
ctx = ctypes.c_void_p(); libcuda.cuInit(0); libcuda.cuCtxCreate_v2(ctypes.byref(ctx), 0, 0)
dptr = ctypes.c_void_p(); libcuda.cuMemAlloc_v2(ctypes.byref(dptr), 1<<20)
libcuda.cuMemsetD8_v2(dptr, 0x42, 1<<20)
print(\"dptr=\", hex(dptr.value or 0), flush=True)
while True: time.sleep(3600)
"'

for _ in $(seq 1 30); do docker logs "$CTR" 2>&1 | grep -q dptr= && break; sleep 3; done
docker logs "$CTR" 2>&1 | tail -3

CID=$(docker ps --filter name="$CTR" -q --no-trunc)
DRV=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1)
sudo rm -rf "$CP" && sudo mkdir -p "$CP"

sudo /usr/local/bin/runsc \
  --nvproxy --nvproxy-driver-version="$DRV" --nvproxy-allowed-driver-capabilities=compute,utility \
  --root=/var/run/docker/runtime-runc/moby \
  checkpoint --image-path="$CP" \
  --save-restore-exec-argv=/usr/local/bin/cuda-checkpoint-wrapper.sh \
  --save-restore-exec-timeout=30s \
  "$CID"

sudo ls -la "$CP"
echo "PASS: runsc checkpoint succeeded with wrapper-driven cuda-checkpoint --toggle."
