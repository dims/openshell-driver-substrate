// seccheck-server is a minimal consumer for gVisor's `remote` trace sink.
//
// It exists to answer one question: can an out-of-sandbox process observe a
// gVisor sandbox's syscalls through seccheck, on a live Substrate actor? That
// is the observation half of the mechanism OpenShell would need in place of
// seccomp user notification. The deny half is a separate change in the sentry:
// seccheck sinks already return an error that aborts the checked operation at
// the execve, clone and mmap checkpoints, but the four generic syscall points
// in task_syscall.go discard it.
//
// The protocol (pkg/sentry/seccheck/sinks/remote) is a SOCK_SEQPACKET unix
// socket. The sink connects, sends a Handshake protobuf, expects one back,
// then sends one message per point: an 8-byte wire.Header followed by a
// protobuf payload. Only the header is decoded here; counting points by type
// is enough to prove the path works, and it keeps this free of a protobuf
// dependency.
package main

import (
	"encoding/binary"
	"fmt"
	"os"
	"os/signal"
	"sort"
	"syscall"
	"time"
)

// handshake is a wire.Handshake protobuf carrying Version=1: field 1, varint,
// value 1. Hand-encoded so this stays dependency-free.
var handshake = []byte{0x08, 0x01}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: seccheck-server <socket-path>")
		os.Exit(2)
	}
	path := os.Args[1]
	_ = os.Remove(path)

	fd, err := syscall.Socket(syscall.AF_UNIX, syscall.SOCK_SEQPACKET, 0)
	if err != nil {
		fmt.Printf("SECCHECK socket: FAIL %v\n", err)
		os.Exit(1)
	}
	if err := syscall.Bind(fd, &syscall.SockaddrUnix{Name: path}); err != nil {
		fmt.Printf("SECCHECK bind %s: FAIL %v\n", path, err)
		os.Exit(1)
	}
	if err := syscall.Listen(fd, 8); err != nil {
		fmt.Printf("SECCHECK listen: FAIL %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("SECCHECK listening on %s\n", path)

	// Report totals on exit so a timed run always prints a summary.
	counts := map[uint16]int{}
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sig
		report(counts)
		os.Exit(0)
	}()
	go func() {
		time.Sleep(50 * time.Second)
		report(counts)
		os.Exit(0)
	}()

	for {
		conn, _, err := syscall.Accept(fd)
		if err != nil {
			continue
		}
		fmt.Println("SECCHECK connection accepted")
		go serve(conn, counts)
	}
}

func serve(conn int, counts map[uint16]int) {
	defer syscall.Close(conn)
	buf := make([]byte, 1<<16)

	n, err := syscall.Read(conn, buf)
	if err != nil || n == 0 {
		fmt.Printf("SECCHECK handshake read: FAIL %v\n", err)
		return
	}
	if _, err := syscall.Write(conn, handshake); err != nil {
		fmt.Printf("SECCHECK handshake write: FAIL %v\n", err)
		return
	}
	fmt.Println("SECCHECK handshake: OK")

	for {
		n, err := syscall.Read(conn, buf)
		if err != nil {
			fmt.Printf("SECCHECK read: %v\n", err)
			return
		}
		if n == 0 {
			fmt.Println("SECCHECK connection closed")
			return
		}
		if n < 8 {
			continue
		}
		msgType := binary.LittleEndian.Uint16(buf[2:4])
		dropped := binary.LittleEndian.Uint32(buf[4:8])
		if counts[msgType] == 0 {
			fmt.Printf("SECCHECK first point: type=%d bytes=%d dropped=%d\n", msgType, n, dropped)
		}
		counts[msgType]++
	}
}

func report(counts map[uint16]int) {
	total := 0
	types := make([]int, 0, len(counts))
	for t, c := range counts {
		types = append(types, int(t))
		total += c
	}
	sort.Ints(types)
	for _, t := range types {
		fmt.Printf("SECCHECK points type=%d count=%d\n", t, counts[uint16(t)])
	}
	fmt.Printf("SECCHECK total points=%d\n", total)
}
