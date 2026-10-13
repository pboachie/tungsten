// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"time"
)

// The cross-process lease of a FileIdempotencyStore (<path>.lock), the same
// file in every runtime: created exclusively, it records its owner
// {"version": 1, "host", "pid", "token"}. A busy lease is waited for up to
// leaseWait. It is reclaimed only from an owner that is provably dead (same
// host, another process, and that process no longer runs); any other owner
// (alive, on another host, or unknown) makes Reserve refuse after the wait.
// The holder re-checks its token just before it commits the store file, so a
// writer whose lease was taken over never overwrites the new owner's key.

// leaseWait is how long Reserve waits for another writer's lease.
const leaseWait = 2 * time.Second

type leaseOwner struct {
	Version int    `json:"version"`
	Host    string `json:"host"`
	PID     int    `json:"pid"`
	Token   string `json:"token"`
}

func leaseHost() string {
	host, err := os.Hostname()
	if err != nil {
		return ""
	}
	return host
}

// readLeaseOwner reads the owner of lease: nil when it cannot be read or
// parsed (still being written, or not a lease), with fs.ErrNotExist when the
// lease is gone.
func readLeaseOwner(lease string) (*leaseOwner, error) {
	data, err := os.ReadFile(lease)
	if err != nil {
		return nil, err
	}
	var owner leaseOwner
	if json.Unmarshal(data, &owner) != nil || owner.Version != 1 || owner.Host == "" || owner.PID <= 0 || owner.Token == "" {
		return nil, nil
	}
	return &owner, nil
}

// provablyDead reports whether the owner's process is known to have ended.
func provablyDead(owner *leaseOwner) bool {
	host := leaseHost()
	return host != "" && owner.Host == host && owner.PID != os.Getpid() && processGone(owner.PID)
}

// acquireLease takes the lease and returns its token.
func acquireLease(lease string) (string, error) {
	b, ok := randomBytes(16)
	if !ok {
		return "", errors.New("no randomness for the idempotency store lease")
	}
	token := hex.EncodeToString(b)
	mine, err := json.Marshal(leaseOwner{Version: 1, Host: leaseHost(), PID: os.Getpid(), Token: token})
	if err != nil {
		return "", err
	}
	deadline := time.Now().Add(leaseWait)
	for delay := 5 * time.Millisecond; ; {
		f, err := os.OpenFile(lease, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
		if err == nil {
			_, err = f.Write(append(mine, '\n'))
			if cerr := f.Close(); err == nil {
				err = cerr
			}
			if err != nil {
				_ = os.Remove(lease)
				return "", err
			}
			return token, nil
		}
		if !errors.Is(err, fs.ErrExist) {
			return "", err
		}
		owner, rerr := readLeaseOwner(lease)
		retryNow := errors.Is(rerr, fs.ErrNotExist)
		if owner != nil && provablyDead(owner) {
			retryNow = reclaimLease(lease, owner, token)
		}
		left := time.Until(deadline)
		if left <= 0 {
			return "", leaseRefusal(lease, owner)
		}
		if !retryNow {
			time.Sleep(min(delay, left))
			delay = min(delay*2, 100*time.Millisecond)
		}
	}
}

// reclaimLease removes a dead owner's lease. It moves the lease aside first:
// if what it moved is not the dead owner's (a live writer took the lease in
// between), it puts it back. It reports whether to retry at once.
func reclaimLease(lease string, dead *leaseOwner, token string) bool {
	aside := lease + "." + token + ".stale"
	if err := os.Rename(lease, aside); err != nil {
		return errors.Is(err, fs.ErrNotExist)
	}
	defer func() { _ = os.Remove(aside) }()
	moved, _ := readLeaseOwner(aside)
	if moved != nil && moved.Token == dead.Token {
		return true
	}
	_ = os.Link(aside, lease)
	return false
}

func leaseRefusal(lease string, owner *leaseOwner) error {
	if owner == nil {
		return fmt.Errorf("the idempotency store is locked by another writer (%s); try again", lease)
	}
	return fmt.Errorf("the idempotency store is locked by another writer (%s, process %d on %s); try again, or remove the lease if that process no longer runs", lease, owner.PID, owner.Host)
}

// ownsLease reports whether lease still holds token.
func ownsLease(lease, token string) bool {
	owner, _ := readLeaseOwner(lease)
	return owner != nil && owner.Token == token
}

// leaseFence fails when the lease was taken over: the key is then not stored.
func leaseFence(lease, token string) func() error {
	return func() error {
		if !ownsLease(lease, token) {
			return fmt.Errorf("the idempotency store lease %s was taken over by another writer; the key was not stored", lease)
		}
		return nil
	}
}

// releaseLease removes the lease if it is still this writer's.
func releaseLease(lease, token string) {
	if ownsLease(lease, token) {
		_ = os.Remove(lease)
	}
}
