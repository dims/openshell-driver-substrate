#!/usr/bin/env bash
# Apply the demonstration patch to a gVisor checkout.
#
# Two changes:
#   1. Add the `deny` sink (this directory).
#   2. Make the generic syscall-enter checkpoint honor a sink's error, which
#      the sentry already does at the execve, clone and mmap checkpoints.
set -o errexit -o nounset -o pipefail
[[ $# -eq 1 ]] || { echo "usage: $0 <gvisor-checkout>" >&2; exit 1; }
GVISOR=$1
HERE=$(cd "$(dirname "$0")" && pwd)

mkdir -p "${GVISOR}/pkg/sentry/seccheck/sinks/deny"
cp "${HERE}/deny.go" "${HERE}/BUILD" "${GVISOR}/pkg/sentry/seccheck/sinks/deny/"

python3 - "$GVISOR" <<'PY'
import sys
gv = sys.argv[1]

# Link the sink in.
p = f"{gv}/runsc/boot/seccheck.go"
s = open(p).read()
anchor = '\t_ "gvisor.dev/gvisor/pkg/sentry/seccheck/sinks/null"\n'
add = '\t_ "gvisor.dev/gvisor/pkg/sentry/seccheck/sinks/deny"\n'
if add not in s:
    s = s.replace(anchor, add + anchor, 1)
    open(p, "w").write(s)

# Honor the sink verdict at the raw syscall-enter checkpoint.
p = f"{gv}/pkg/sentry/kernel/task_syscall.go"
s = open(p).read()
old = """		seccheck.Global.SentToSinks(func(c seccheck.Sink) error {
			return c.RawSyscall(t, fields, &info)
		})
	}
	if bits.IsAnyOn(fe, SecCheckEnter) {"""
new = """		if err := seccheck.Global.SentToSinks(func(c seccheck.Sink) error {
			return c.RawSyscall(t, fields, &info)
		}); err != nil {
			// A sink refused the syscall. Skip it and return the sink's errno,
			// the same shape the mmap checkpoint already has.
			return 0, nil, err
		}
	}
	if bits.IsAnyOn(fe, SecCheckEnter) {"""
if new not in s:
    assert old in s, "syscall-enter checkpoint not found; gVisor moved"
    s = s.replace(old, new, 1)
    open(p, "w").write(s)
PY

# Bazel needs the new dep declared on runsc/boot.
python3 - "$GVISOR" <<'PY'
import sys
gv = sys.argv[1]
p = f"{gv}/runsc/boot/BUILD"
s = open(p).read()
anchor = '"//pkg/sentry/seccheck/sinks/null",'
add = '"//pkg/sentry/seccheck/sinks/deny",\n        '
if "sinks/deny" not in s:
    s = s.replace(anchor, add + anchor, 1)
    open(p, "w").write(s)
PY

echo "patched ${GVISOR}"
