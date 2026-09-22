package main

import (
	"fmt"
	"os"
	"strings"
	"syscall"
	"unsafe"
)

const (
	sysProcessVMReadv = 310 // amd64
	sysSeccomp        = 317 // amd64
	prSetDumpable     = 4
	prGetDumpable     = 3
	setModeFilter     = 1
	flagNewListener   = 1 << 3
	retAllow          = 0x7fff0000
)

type iovec struct {
	base *byte
	len  uint64
}

type sockFilter struct {
	code uint16
	jt   uint8
	jf   uint8
	k    uint32
}

type sockFprog struct {
	len    uint16
	_      [6]byte
	filter *sockFilter
}

// vmReadv reads len(dst) bytes from pid's address space at src.
func vmReadv(pid int, src *byte, dst []byte) error {
	l := iovec{base: &dst[0], len: uint64(len(dst))}
	r := iovec{base: src, len: uint64(len(dst))}
	n, _, errno := syscall.Syscall6(sysProcessVMReadv, uintptr(pid),
		uintptr(unsafe.Pointer(&l)), 1, uintptr(unsafe.Pointer(&r)), 1, 0)
	if errno != 0 {
		return errno
	}
	if int(n) != len(dst) {
		return fmt.Errorf("short read %d", n)
	}
	return nil
}

func tryVMReadSelf(label string) {
	src := []byte("SENTINEL")
	dst := make([]byte, len(src))
	if err := vmReadv(os.Getpid(), &src[0], dst); err != nil {
		fmt.Printf("PROBE %s process_vm_readv(self): FAIL %v (errno=%d)\n", label, err, errnoOf(err))
		return
	}
	fmt.Printf("PROBE %s process_vm_readv(self): OK got=%q\n", label, string(dst))
}

func tryVMReadOther(label string, pid int) {
	// Reading another task needs a valid remote address; use a deliberately
	// bogus one. EFAULT means access was granted and only the address was bad;
	// EPERM/EACCES means access itself was refused.
	dst := make([]byte, 8)
	err := vmReadv(pid, (*byte)(unsafe.Pointer(uintptr(0x1000))), dst)
	fmt.Printf("PROBE %s process_vm_readv(pid=%d): %v (errno=%d)\n", label, pid, err, errnoOf(err))
}

func tryOpenMem(label string) {
	for _, t := range []string{"/proc/self/mem", "/proc/1/mem"} {
		fd, err := os.Open(t)
		if err != nil {
			fmt.Printf("PROBE %s open %s: FAIL %v\n", label, t, err)
			continue
		}
		fmt.Printf("PROBE %s open %s: OK\n", label, t)
		fd.Close()
	}
}

func trySeccomp(flags uintptr, label string) {
	prog := []sockFilter{{code: 0x06, k: retAllow}}
	fp := sockFprog{len: uint16(len(prog)), filter: &prog[0]}
	r, _, errno := syscall.Syscall(sysSeccomp, setModeFilter, flags, uintptr(unsafe.Pointer(&fp)))
	if errno != 0 {
		fmt.Printf("PROBE %s: FAIL errno=%d (%v)\n", label, int(errno), errno)
		return
	}
	fmt.Printf("PROBE %s: OK (ret=%d)\n", label, int(r))
}

func dumpable() int {
	r, _, _ := syscall.Syscall(syscall.SYS_PRCTL, prGetDumpable, 0, 0)
	return int(r)
}

func errnoOf(err error) int {
	if e, ok := err.(syscall.Errno); ok {
		return int(e)
	}
	return -1
}

func main() {
	if b, err := os.ReadFile("/proc/version"); err == nil {
		fmt.Printf("PROBE kernel: %s\n", strings.TrimSpace(string(b)))
	}
	var u syscall.Utsname
	if err := syscall.Uname(&u); err == nil {
		b := make([]byte, 0, 65)
		for _, c := range u.Release {
			if c == 0 {
				break
			}
			b = append(b, byte(c))
		}
		fmt.Printf("PROBE uname.release=%s\n", string(b))
	}
	fmt.Printf("PROBE uid=%d gid=%d\n", os.Getuid(), os.Getgid())
	if b, err := os.ReadFile("/proc/self/status"); err == nil {
		for _, l := range strings.Split(string(b), "\n") {
			for _, k := range []string{"NoNewPrivs", "CapBnd", "CapEff", "Seccomp"} {
				if strings.HasPrefix(l, k+":") {
					fmt.Printf("PROBE %s\n", strings.Join(strings.Fields(l), " "))
				}
			}
		}
	}

	fmt.Printf("PROBE dumpable=%d\n", dumpable())
	tryOpenMem("[dumpable]")
	tryVMReadSelf("[dumpable]")
	tryVMReadOther("[dumpable]", 1)

	if _, _, e := syscall.Syscall(syscall.SYS_PRCTL, prSetDumpable, 0, 0); e != 0 {
		fmt.Printf("PROBE set-nondumpable: FAIL %v\n", e)
	} else {
		fmt.Printf("PROBE set-nondumpable: OK dumpable=%d\n", dumpable())
	}

	tryOpenMem("[nondumpable]")
	tryVMReadSelf("[nondumpable]")
	tryVMReadOther("[nondumpable]", 1)
	trySeccomp(flagNewListener, "seccomp+NEW_LISTENER [nondumpable]")

	fmt.Println("PROBE done")
	select {}
}
