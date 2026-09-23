// redirect-probe proves the egress capture OpenShell needs under gVisor,
// where seccomp user notification does not exist.
//
// Two modes, run as two containers of one actor so they share the sentry's
// netstack:
//
//   - install: root plus NET_ADMIN. Installs the nat REDIRECT rule that bends
//     the workload's TCP to the sandbox, and the filter rule that drops
//     anything the redirect did not catch. Stands in for the supervisor.
//   - capture: the workload identity, capability-free. Listens on the redirect
//     port, connects out, and reports what arrived. Stands in for the sandbox
//     plus its workload.
//
// A passing capture run shows three things: a privileged sibling's rules bind
// a capability-free sibling's traffic, the original destination survives the
// redirect, and traffic the redirect misses does not leave.
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

const (
	// Unroutable by design (TEST-NET-3). Nothing listens and nothing routes,
	// so a connection can only succeed by being redirected.
	targetIP = "203.0.113.7"
	// Redirected to the sandbox.
	capturedPort = 9999
	// Deliberately has no redirect rule, so the fence is what decides it.
	uncapturedPort = 9998
	// Control: redirected with no owner match, to tell "the owner match did
	// not fire" apart from "the redirect did not fire".
	controlPort = 9997
	proxyPort   = 18515
	// A sibling-reachability check that needs no capabilities.
	peerProbePort = 18516
	// Same-container control: the installer redirects and captures its own
	// traffic, which is the arrangement already known to work. If this fires
	// and the sibling's does not, the boundary is the container, not the rule.
	selfPort      = 9996
	selfProxyPort = 18517
)

const (
	solIP         = 0
	soOriginalDst = 80
)

// workloadUID is the identity the capture container runs as, and the one the
// owner match names. The installer runs as root and is deliberately exempt:
// the supervisor has to reach the network the workload cannot.
const workloadUID = "65532"

// report writes to stdout and to the durable volume. Stdout alone is not
// enough: after a checkpoint restore the container's stdout is the descriptor
// captured in the image, which no longer reaches the log pipe, so a restored
// probe looks silent. The volume is a host directory and survives.
func report(format string, args ...any) {
	line := fmt.Sprintf(format, args...)
	fmt.Println(line)
	// One file per role. The two containers run as different identities and
	// the volume is shared, so a single file would be created by whichever
	// started first and be unwritable by the other.
	path := fmt.Sprintf("/out/probe-%s.log", os.Getenv("REDIR_MODE"))
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		fmt.Printf("report to %s failed: %v\n", path, err)
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "%s %s\n", time.Now().UTC().Format(time.RFC3339), line)
}

func run(name string, args ...string) (string, error) {
	cmd := exec.Command(name, args...)
	// /run is root-owned, so a non-root container cannot create the default
	// lock file and iptables refuses to run at all, even to list.
	cmd.Env = append(os.Environ(), "XTABLES_LOCKFILE=/tmp/xtables.lock")
	out, err := cmd.CombinedOutput()
	return strings.TrimSpace(string(out)), err
}

// installRules puts the capture and fence rules in the sandbox's netstack.
func installRules() {
	rules := [][]string{
		// Bend captured TCP to the sandbox. Only the workload identity: the
		// supervisor's own egress must not loop back into itself.
		{"-t", "nat", "-A", "OUTPUT", "-p", "tcp", "-m", "owner", "--uid-owner", workloadUID,
			"--dport", fmt.Sprint(capturedPort), "-j", "REDIRECT", "--to-port", fmt.Sprint(proxyPort)},
		// Control, no owner match.
		{"-t", "nat", "-A", "OUTPUT", "-p", "tcp",
			"--dport", fmt.Sprint(controlPort), "-j", "REDIRECT", "--to-port", fmt.Sprint(proxyPort)},
		// Same-container control.
		{"-t", "nat", "-A", "OUTPUT", "-p", "tcp",
			"--dport", fmt.Sprint(selfPort), "-j", "REDIRECT", "--to-port", fmt.Sprint(selfProxyPort)},
		// The fence. REDIRECT has already rewritten a captured packet's
		// destination to loopback by the time the filter chain sees it, so
		// "not loopback" is exactly "the redirect did not catch this".
		{"-A", "OUTPUT", "-m", "owner", "--uid-owner", workloadUID,
			"!", "-d", "127.0.0.0/8", "-j", "DROP"},
	}
	for _, rule := range rules {
		out, err := run("iptables-legacy", rule...)
		if err != nil {
			report("REDIR install %v: FAIL %v: %s", rule, err, out)
			return
		}
	}
	report("REDIR install rules: OK")
	listing, _ := run("iptables-legacy", "-t", "nat", "-S", "OUTPUT")
	report("REDIR nat OUTPUT: %s", strings.ReplaceAll(listing, "\n", " | "))
}

// originalDst recovers the address a redirected connection was opened to.
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
	if err := raw.Control(func(fd uintptr) {
		_, _, errno = syscall.Syscall6(syscall.SYS_GETSOCKOPT, fd,
			solIP, soOriginalDst,
			uintptr(unsafe.Pointer(&addr)), uintptr(unsafe.Pointer(&size)), 0)
	}); err != nil {
		return "", err
	}
	if errno != 0 {
		return "", errno
	}
	port := int(addr.Port>>8) | int(addr.Port&0xff)<<8
	return fmt.Sprintf("%d.%d.%d.%d:%d",
		addr.Addr[0], addr.Addr[1], addr.Addr[2], addr.Addr[3], port), nil
}

// captureRound dials the captured port and reports what the listener saw.
func captureRound(ln *net.TCPListener, round int, port int, label string) {
	// Bound the accept. When the dial fails there is nothing to accept, and an
	// unbounded Accept would wedge the round forever.
	_ = ln.SetDeadline(time.Now().Add(8 * time.Second))
	done := make(chan struct{})
	go func() {
		defer close(done)
		c, err := ln.AcceptTCP()
		if err != nil {
			report("REDIR #%d %s accept: FAIL %v", round, label, err)
			return
		}
		defer c.Close()
		dst, err := originalDst(c)
		if err != nil {
			report("REDIR #%d %s SO_ORIGINAL_DST: FAIL %v", round, label, err)
			return
		}
		want := fmt.Sprintf("%s:%d", targetIP, port)
		if dst != want {
			report("REDIR #%d %s SO_ORIGINAL_DST: WRONG got=%s want=%s", round, label, dst, want)
			return
		}
		report("REDIR #%d %s captured: OK original=%s", round, label, dst)
	}()

	c, err := net.DialTimeout("tcp", fmt.Sprintf("%s:%d", targetIP, port), 5*time.Second)
	if err != nil {
		report("REDIR #%d %s dial: FAIL %v", round, label, err)
	}
	<-done
	if err == nil {
		c.Close()
	}
}

// peerRound asks whether a sibling container's loopback listener is reachable,
// which is the same question as whether the two share a network namespace.
// Retried every round because the two containers start together and the
// sibling may not have bound yet.
func peerRound(round int) {
	c, err := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", peerProbePort), 4*time.Second)
	if err == nil {
		c.Close()
		report("REDIR #%d netns shared: YES, reached sibling loopback listener", round)
		return
	}
	report("REDIR #%d netns shared: NO (%v)", round, err)
}

// fenceRound dials a port with no redirect rule. The fence must stop it.
func fenceRound(round int) {
	c, err := net.DialTimeout("tcp", fmt.Sprintf("%s:%d", targetIP, uncapturedPort), 3*time.Second)
	if err == nil {
		c.Close()
		report("REDIR #%d fence: LEAKED, uncaptured connection succeeded", round)
		return
	}
	report("REDIR #%d fence: OK uncaptured blocked (%v)", round, err)
}

func main() {
	mode := os.Getenv("REDIR_MODE")
	netns, _ := os.Readlink("/proc/self/ns/net")
	report("REDIR mode=%s uid=%d netns=%s", mode, os.Getuid(), netns)

	// Whether a sibling's rules bind this container's traffic is the whole
	// question. Installing them here too isolates "the mechanism does not
	// work" from "the mechanism does not cross a container boundary".
	if os.Getenv("REDIR_SELF_INSTALL") == "1" {
		installRules()
	}

	// The shape the real sandbox uses: install the rules while still root,
	// then drop to the workload identity with no capabilities and carry on in
	// the same container. gVisor keeps netfilter state per container, so the
	// rules installed above this line still bind the traffic below it.
	if mode == "install-then-drop" {
		installRules()
		// One round as root first, with the same rule the child will use, so
		// the only variable left between the two is the identity.
		rootLn, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4zero, Port: proxyPort})
		if err != nil {
			report("REDIR root listen: FAIL %v", err)
		} else {
			captureRound(rootLn, 0, controlPort, "root-before-drop")
			rootLn.Close()
		}

		report("REDIR dropping to %s with no capabilities", workloadUID)
		// Only the identity change. Clearing the bounding set would need
		// CAP_SETPCAP, which this container does not have, and is unnecessary:
		// a non-root process keeps no permitted or effective capabilities
		// without ambient ones, and Substrate never sets those.
		cmd := exec.Command("setpriv",
			"--reuid", workloadUID, "--regid", workloadUID, "--clear-groups",
			"/redirect-probe")
		cmd.Env = append(os.Environ(), "REDIR_MODE=capture")
		out, err := cmd.CombinedOutput()
		report("REDIR setpriv exited: err=%v out=%s", err, strings.TrimSpace(string(out)))
		hold()
	}

	if mode == "install" {
		installRules()
		// A plain loopback listener. Whether a sibling can reach it settles
		// whether the two containers share one network namespace, which no
		// amount of rule-watching can settle from a container with no
		// capabilities.
		go func() {
			ln, err := net.Listen("tcp4", fmt.Sprintf("127.0.0.1:%d", peerProbePort))
			if err != nil {
				report("REDIR peer listener: FAIL %v", err)
				return
			}
			report("REDIR peer listener on 127.0.0.1:%d", peerProbePort)
			for {
				c, err := ln.Accept()
				if err != nil {
					continue
				}
				report("REDIR peer listener: accepted from %s", c.RemoteAddr())
				c.Close()
			}
		}()
		// Same-container capture control, on its own port so it does not
		// collide with the sibling's listener in the shared namespace.
		selfLn, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4zero, Port: selfProxyPort})
		if err != nil {
			report("REDIR self listen: FAIL %v", err)
			hold()
		}
		for round := 0; ; round++ {
			captureRound(selfLn, round, selfPort, "self(same-container)")
			time.Sleep(20 * time.Second)
		}
	}



	// "tcp4", not "tcp". A wildcard listen gives a dual-stack AF_INET6 socket,
	// and gVisor resolves SO_ORIGINAL_DST through conntrack keyed on the
	// socket's protocol rather than the connection's, so the lookup misses and
	// the option reads back ENOTCONN.
	ln, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4zero, Port: proxyPort})
	if err != nil {
		report("REDIR listen: FAIL %v", err)
		hold()
	}
	// Whether this container sees the installer's rules is the question: a
	// shared table means one netfilter state for the sandbox, a bare policy
	// line means each container got its own.
	listing, err := run("iptables-legacy", "-t", "nat", "-S", "OUTPUT")
	report("REDIR visible nat OUTPUT (err=%v): %s", err, strings.ReplaceAll(listing, "\n", " | "))
	report("REDIR listening on :%d", proxyPort)

	// The installer is a sibling container with no ordering guarantee, so the
	// first rounds may run before its rules exist.
	for round := 0; ; round++ {
		peerRound(round)
		captureRound(ln, round, controlPort, "control(no-owner)")
		captureRound(ln, round, capturedPort, "owner-match")
		fenceRound(round)
		time.Sleep(20 * time.Second)
	}
}

// hold keeps the container alive so the actor can be snapshotted and its logs
// read; a container that exits takes its actor down with it. A bare select{}
// does not work: the Go runtime spots that every goroutine is asleep and
// aborts with "all goroutines are asleep - deadlock!".
func hold() {
	for {
		time.Sleep(time.Hour)
	}
}
