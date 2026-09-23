// redirect-probe tests whether a transparent proxy can be built inside a
// gVisor sandbox without seccomp user notification.
//
// OpenShell's native Linux backend virtualizes sockets by intercepting connect
// with a seccomp notification and injecting a replacement fd through
// SECCOMP_IOCTL_NOTIF_ADDFD. gVisor implements neither. The classic
// transparent-proxy primitives are an alternative that needs no notification:
// an iptables nat REDIRECT rule bends the connection to a local listener, and
// the listener recovers the address the client asked for with SO_ORIGINAL_DST.
//
// The probe installs the rule, connects to an address nothing is listening on,
// and reports what the accepting side sees. A successful run proves both halves
// work in the sentry's netstack.
package main

import (
	"fmt"
	"net"
	"os"
	"os/exec"
	"strings"
	"syscall"
	"time"
	"unsafe"
)

// The victim address. Nothing listens here; the REDIRECT rule is the only
// reason a connection to it can succeed.
const (
	targetIP   = "203.0.113.7"
	targetPort = 9999
	proxyPort  = 18080
)

const (
	solIP         = 0
	soOriginalDst = 80
)

func step(name string, err error) bool {
	if err != nil {
		fmt.Printf("REDIR %s: FAIL %v\n", name, err)
		return false
	}
	fmt.Printf("REDIR %s: OK\n", name)
	return true
}

// originalDst recovers the pre-REDIRECT destination of an accepted connection.
func originalDst(c *net.TCPConn) (string, error) {
	raw, err := c.SyscallConn()
	if err != nil {
		return "", err
	}
	var (
		addr  syscall.RawSockaddrInet4
		errno syscall.Errno
	)
	size := uint32(unsafe.Sizeof(addr))
	err = raw.Control(func(fd uintptr) {
		_, _, errno = syscall.Syscall6(syscall.SYS_GETSOCKOPT, fd,
			solIP, soOriginalDst,
			uintptr(unsafe.Pointer(&addr)), uintptr(unsafe.Pointer(&size)), 0)
	})
	if err != nil {
		return "", err
	}
	if errno != 0 {
		return "", errno
	}
	port := int(addr.Port>>8) | int(addr.Port&0xff)<<8
	return fmt.Sprintf("%d.%d.%d.%d:%d",
		addr.Addr[0], addr.Addr[1], addr.Addr[2], addr.Addr[3], port), nil
}

// iptGetInfo issues the getsockopt libiptc uses to open a table, so a failure
// is reported as a real errno instead of iptables' misleading "Table does not
// exist", which is what iptc_strerror prints for ENOPROTOOPT.
func iptGetInfo(table string) error {
	const iptSOGetInfo = 64
	fd, err := syscall.Socket(syscall.AF_INET, syscall.SOCK_RAW, syscall.IPPROTO_RAW)
	if err != nil {
		return fmt.Errorf("socket(AF_INET, SOCK_RAW, IPPROTO_RAW): %w", err)
	}
	defer syscall.Close(fd)

	// struct ipt_getinfo: name[32] + valid_hooks + hook_entry[5] + underflow[5]
	// + num_entries + size.
	buf := make([]byte, 84)
	copy(buf, table)
	size := uint32(len(buf))
	_, _, errno := syscall.Syscall6(syscall.SYS_GETSOCKOPT, uintptr(fd),
		solIP, iptSOGetInfo,
		uintptr(unsafe.Pointer(&buf[0])), uintptr(unsafe.Pointer(&size)), 0)
	if errno != 0 {
		return fmt.Errorf("getsockopt(IPT_SO_GET_INFO, %q): %w", table, errno)
	}
	return nil
}

func main() {
	fmt.Printf("REDIR uid=%d\n", os.Getuid())
	if b, err := os.ReadFile("/proc/self/status"); err == nil {
		for _, l := range strings.Split(string(b), "\n") {
			if strings.HasPrefix(l, "CapEff:") || strings.HasPrefix(l, "CapBnd:") {
				fmt.Printf("REDIR %s\n", strings.Join(strings.Fields(l), " "))
			}
		}
	}
	for _, tbl := range []string{"filter", "nat"} {
		if err := iptGetInfo(tbl); err != nil {
			fmt.Printf("REDIR IPT_SO_GET_INFO %s: FAIL %v\n", tbl, err)
		} else {
			fmt.Printf("REDIR IPT_SO_GET_INFO %s: OK\n", tbl)
		}
	}

	ln, err := net.ListenTCP("tcp", &net.TCPAddr{IP: net.IPv4zero, Port: proxyPort})
	if !step("listen proxy", err) {
		hold()
	}

	// OUTPUT rather than PREROUTING: the connection originates in this netns.
	rule := []string{"-t", "nat", "-A", "OUTPUT", "-p", "tcp",
		"--dport", fmt.Sprint(targetPort), "-j", "REDIRECT", "--to-port", fmt.Sprint(proxyPort)}
	// iptables-legacy first. gVisor's netfilter is the legacy xtables
	// setsockopt interface; the nft backend most distributions now default to
	// fails with "Failed to initialize nft: Protocol not supported".
	var installed bool
	for _, bin := range []string{"iptables-legacy", "iptables"} {
		out, err := exec.Command(bin, rule...).CombinedOutput()
		if err == nil {
			fmt.Printf("REDIR install nat REDIRECT (%s): OK\n", bin)
			installed = true
			break
		}
		fmt.Printf("REDIR install nat REDIRECT (%s): FAIL %v: %s\n", bin, err, out)
	}
	if !installed {
		hold()
	}

	done := make(chan struct{})
	go func() {
		defer close(done)
		c, err := ln.AcceptTCP()
		if !step("accept redirected conn", err) {
			return
		}
		defer c.Close()
		dst, err := originalDst(c)
		if err != nil {
			fmt.Printf("REDIR SO_ORIGINAL_DST: FAIL %v\n", err)
			return
		}
		want := fmt.Sprintf("%s:%d", targetIP, targetPort)
		if dst != want {
			fmt.Printf("REDIR SO_ORIGINAL_DST: WRONG got=%s want=%s\n", dst, want)
			return
		}
		fmt.Printf("REDIR SO_ORIGINAL_DST: OK got=%s\n", dst)
	}()

	c, err := net.Dial("tcp", fmt.Sprintf("%s:%d", targetIP, targetPort))
	ok := step("connect to unrouted target", err)
	// Hold the client open until the server has read SO_ORIGINAL_DST. Closing
	// first tears the endpoint down and the option reads back ENOTCONN.
	<-done
	if ok {
		c.Close()
	}

	fmt.Println("REDIR done")
	hold()
}

// hold keeps the container alive so the actor can be snapshotted and the logs
// read; a container that exits takes its actor down with it. A bare select{}
// does not work: the Go runtime spots that every goroutine is asleep and aborts
// with "all goroutines are asleep - deadlock!".
func hold() {
	for {
		time.Sleep(time.Hour)
	}
}
