// deny-probe calls one syscall on a loop and reports what happened.
//
// It is the workload for the seccheck mediation demonstration: attach a trace
// session carrying the `deny` sink while this is running and the same call in
// the same process starts failing. uname is used because it is harmless, takes
// no arguments worth validating, and nothing else in the sandbox depends on it.
package main

import (
	"fmt"
	"syscall"
	"time"
)

func main() {
	fmt.Println("DENY probe started, calling uname (sysno 63) every 2s")
	for i := 0; ; i++ {
		var u syscall.Utsname
		err := syscall.Uname(&u)
		if err != nil {
			fmt.Printf("DENY uname #%d: FAIL %v\n", i, err)
		} else {
			fmt.Printf("DENY uname #%d: OK\n", i)
		}
		time.Sleep(2 * time.Second)
	}
}
