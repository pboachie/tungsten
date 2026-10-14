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
// leaseWait, then Reserve refuses, naming the owner. Only its owner removes a
// lease: no writer reclaims another's, because a reclaimer cannot remove a
// lease and check whose it was in one step (a delayed reclaimer could remove
// a newer live lease). A lease left by a process that died is removed by
// hand. The holder re-checks its token just before it commits the store
// file, so a writer whose lease was removed never overwrites a key.

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
// parsed (still being written, or not a lease).
func readLeaseOwner(lease string) *leaseOwner {
	data, err := os.ReadFile(lease)
	if err != nil {
		return nil
	}
	var owner leaseOwner
	if json.Unmarshal(data, &owner) != nil || owner.Version != 1 || owner.PID <= 0 || owner.Token == "" {
		return nil
	}
	return &owner
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
		left := time.Until(deadline)
		if left <= 0 {
			return "", leaseRefusal(lease, readLeaseOwner(lease))
		}
		time.Sleep(min(delay, left))
		delay = min(delay*2, 100*time.Millisecond)
	}
}

func leaseRefusal(lease string, owner *leaseOwner) error {
	if owner == nil {
		return fmt.Errorf("the idempotency store is locked by another writer (%s); try again", lease)
	}
	return fmt.Errorf("the idempotency store is locked by another writer (%s, process %d on %q); try again, or remove the lease once that process no longer runs", lease, owner.PID, owner.Host)
}

// ownsLease reports whether lease still holds token.
func ownsLease(lease, token string) bool {
	owner := readLeaseOwner(lease)
	return owner != nil && owner.Token == token
}

// leaseFence fails when the lease is no longer this writer's: the key is then not stored.
func leaseFence(lease, token string) func() error {
	return func() error {
		if !ownsLease(lease, token) {
			return fmt.Errorf("the idempotency store lease %s is no longer this writer's; the key was not stored", lease)
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
