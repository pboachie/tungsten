// SPDX-License-Identifier: Apache-2.0

//go:build unix

package tungsten

import (
	"errors"
	"syscall"
)

// processGone reports whether no process with pid runs on this host.
func processGone(pid int) bool {
	return errors.Is(syscall.Kill(pid, 0), syscall.ESRCH)
}
