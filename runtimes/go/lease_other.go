// SPDX-License-Identifier: Apache-2.0

//go:build !unix

package tungsten

// processGone cannot prove a process ended here, so a lease is never reclaimed.
func processGone(int) bool { return false }
