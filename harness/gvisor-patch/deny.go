// Copyright 2026 The gVisor Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Package deny defines a seccheck.Sink that refuses one syscall number.
//
// It exists to demonstrate that a seccheck sink can mediate, not just observe.
// The Sink interface is already error-returning and the sentry already aborts
// the checked operation at the execve, clone and mmap checkpoints; the generic
// syscall checkpoints discard the verdict instead. With the companion change to
// task_syscall.go, this sink turns a chosen syscall into EPERM for every task in
// the sandbox.
//
// Config: {"sysno": <number>}. Not for production: there is no scoping by
// container, task or argument, and the decision is hardcoded rather than
// delegated to the remote process that a real mediating sink would consult.
package deny

import (
	"fmt"

	"gvisor.dev/gvisor/pkg/context"
	"gvisor.dev/gvisor/pkg/errors/linuxerr"
	"gvisor.dev/gvisor/pkg/fd"
	"gvisor.dev/gvisor/pkg/sentry/seccheck"
	pb "gvisor.dev/gvisor/pkg/sentry/seccheck/points/points_go_proto"
)

const name = "deny"

func init() {
	seccheck.RegisterSink(seccheck.SinkDesc{
		Name: name,
		New:  newSink,
	})
}

type deny struct {
	seccheck.SinkDefaults
	sysno uint64
}

var _ seccheck.Sink = (*deny)(nil)

func newSink(config map[string]any, _ *fd.FD) (seccheck.Sink, error) {
	raw, ok := config["sysno"]
	if !ok {
		return nil, fmt.Errorf("deny sink requires a %q config value", "sysno")
	}
	// JSON numbers decode to float64.
	num, ok := raw.(float64)
	if !ok {
		return nil, fmt.Errorf("deny sink sysno %v is not a number", raw)
	}
	return &deny{sysno: uint64(num)}, nil
}

func (*deny) Name() string {
	return name
}

// RawSyscall implements seccheck.Sink.RawSyscall.
func (d *deny) RawSyscall(_ context.Context, _ seccheck.FieldSet, s *pb.Syscall) error {
	if s.GetSysno() == d.sysno {
		return linuxerr.EPERM
	}
	return nil
}
