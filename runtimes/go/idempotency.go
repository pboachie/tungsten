// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

// MemoryIdempotencyStore keeps keys for the life of the process.
type MemoryIdempotencyStore struct {
	mu   sync.Mutex
	keys map[[2]string]string
}

// NewMemoryIdempotencyStore returns an empty store.
func NewMemoryIdempotencyStore() *MemoryIdempotencyStore {
	return &MemoryIdempotencyStore{keys: map[[2]string]string{}}
}

// Get implements IdempotencyStore.
func (s *MemoryIdempotencyStore) Get(scope, logicalID string) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	k, ok := s.keys[[2]string{scope, logicalID}]
	return k, ok
}

// Put implements IdempotencyStore.
func (s *MemoryIdempotencyStore) Put(scope, logicalID, key string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.keys == nil {
		s.keys = map[[2]string]string{}
	}
	s.keys[[2]string{scope, logicalID}] = key
}

// FileIdempotencyStore keeps keys in a JSON file ({"version": 1, "keys":
// {scope: {id: key}}}, the format of the other runtimes), so a retry after
// a crash reuses the key. Writes are serialized within the process and
// atomic (a temporary file, then a rename); the file is created with mode
// 0600. A file that cannot be parsed makes Get report nothing and Put write
// nothing, and the runtime then refuses the call rather than issue a second
// key for the same intent. Reserve also holds the cross-process lease
// <path>.lock: it waits up to 2 s for another process's lease and breaks one
// older than 30 s (its writer died).
type FileIdempotencyStore struct {
	path string
	mu   sync.Mutex
}

// NewFileIdempotencyStore stores keys at path.
func NewFileIdempotencyStore(path string) *FileIdempotencyStore {
	return &FileIdempotencyStore{path: path}
}

// Path is the file's path.
func (s *FileIdempotencyStore) Path() string { return s.path }

func (s *FileIdempotencyStore) read() (map[string]map[string]string, bool) {
	data, err := os.ReadFile(s.path)
	if errors.Is(err, fs.ErrNotExist) {
		return map[string]map[string]string{}, true
	}
	if err != nil {
		return nil, false
	}
	var doc struct {
		Version *json.Number                 `json:"version"`
		Keys    map[string]map[string]string `json:"keys"`
	}
	if json.Unmarshal(data, &doc) != nil || doc.Version == nil || doc.Version.String() != "1" || doc.Keys == nil {
		return nil, false
	}
	return doc.Keys, true
}

func (s *FileIdempotencyStore) write(data map[string]map[string]string) error {
	keys := NewObject()
	scopes := make([]string, 0, len(data))
	for scope := range data {
		scopes = append(scopes, scope)
	}
	sort.Strings(scopes)
	for _, scope := range scopes {
		entries := NewObject()
		ids := make([]string, 0, len(data[scope]))
		for id := range data[scope] {
			ids = append(ids, id)
		}
		sort.Strings(ids)
		for _, id := range ids {
			entries.Set(id, data[scope][id])
		}
		keys.Set(scope, entries)
	}
	doc := NewObject()
	doc.Set("version", int64(1))
	doc.Set("keys", keys)
	var pretty bytes.Buffer
	if err := json.Indent(&pretty, []byte(JSONText(doc)), "", "  "); err != nil {
		return err
	}
	text := append(pretty.Bytes(), '\n')
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	suffix := "tmp"
	if b, ok := randomBytes(6); ok {
		suffix = hex.EncodeToString(b)
	}
	tmp := filepath.Join(dir, "."+filepath.Base(s.path)+"."+suffix+".tmp")
	f, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	_, err = f.Write(text)
	if err == nil {
		err = f.Sync()
	}
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = os.Rename(tmp, s.path)
	}
	if err != nil {
		_ = os.Remove(tmp)
	}
	return err
}

// Get implements IdempotencyStore.
func (s *FileIdempotencyStore) Get(scope, logicalID string) (string, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	data, ok := s.read()
	if !ok {
		return "", false
	}
	k, ok := data[scope][logicalID]
	return k, ok
}

// Put implements IdempotencyStore.
func (s *FileIdempotencyStore) Put(scope, logicalID, key string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	data, ok := s.read()
	if !ok {
		return
	}
	if data[scope] == nil {
		data[scope] = map[string]string{}
	}
	data[scope][logicalID] = key
	// A failed write is noticed by the caller reading the key back.
	_ = s.write(data)
}

// ----------------------------------------------------------- key formats

func formatKind(format string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(format) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') {
			b.WriteRune(r)
		}
	}
	switch b.String() {
	case "uuidv4", "uuid4":
		return "uuid_v4"
	case "uuid":
		return "uuid"
	case "asciitoken128":
		return "ascii_token_128"
	}
	return "token"
}

// keyFormatDescription describes the key format a policy expects.
func keyFormatDescription(format string) string {
	switch formatKind(format) {
	case "uuid_v4":
		return "UUIDv4 string"
	case "uuid":
		return "UUID string"
	case "ascii_token_128":
		return "1-128 ASCII letters, digits, dots, underscores or hyphens"
	}
	return "a non-empty printable ASCII string of at most 255 characters"
}

func isHex(b byte) bool {
	return (b >= '0' && b <= '9') || (b >= 'a' && b <= 'f') || (b >= 'A' && b <= 'F')
}

func isUUID(key string, v4Only bool) bool {
	if len(key) != 36 {
		return false
	}
	for i := 0; i < len(key); i++ {
		b := key[i]
		var ok bool
		switch i {
		case 8, 13, 18, 23:
			ok = b == '-'
		case 14:
			if v4Only {
				ok = b == '4'
			} else {
				ok = b >= '1' && b <= '8'
			}
		case 19:
			l := b | 0x20
			ok = l == '8' || l == '9' || l == 'a' || l == 'b'
		default:
			ok = isHex(b)
		}
		if !ok {
			return false
		}
	}
	return true
}

// checkKeyFormat returns the expected format when the key does not match.
func checkKeyFormat(key, format string) (string, bool) {
	var valid bool
	switch formatKind(format) {
	case "uuid_v4":
		valid = isUUID(key, true)
	case "uuid":
		valid = isUUID(key, false)
	case "ascii_token_128":
		valid = len(key) >= 1 && len(key) <= 128
		for i := 0; i < len(key) && valid; i++ {
			b := key[i]
			valid = (b >= 'A' && b <= 'Z') || (b >= 'a' && b <= 'z') || (b >= '0' && b <= '9') || b == '.' || b == '_' || b == '-'
		}
	default:
		valid = key != "" && len(key) <= 255
		for i := 0; i < len(key) && valid; i++ {
			valid = key[i] >= 0x21 && key[i] <= 0x7e
		}
	}
	if valid {
		return "", true
	}
	return keyFormatDescription(format), false
}

// keyHeader is the wire header of the operation's key.
func keyHeader(op *OperationDescriptor) string {
	if op.Agent.Idempotency.Header != "" {
		return op.Agent.Idempotency.Header
	}
	for _, p := range op.Params {
		if p.Role == RoleIdempotencyKey {
			return p.Wire
		}
	}
	return "Idempotency-Key"
}

// AtomicIdempotencyStore persists candidate only if there is no existing key.
// Custom cross-process stores should implement this transactional extension.
type AtomicIdempotencyStore interface {
	Reserve(scope, logicalID, candidate string) (string, error)
}

var legacyReservationMu sync.Mutex

// fileReservationLocks serializes the reservations of one store file within
// the process (by absolute path); the lease serializes them across processes.
var fileReservationLocks sync.Map

func fileReservationLock(path string) *sync.Mutex {
	if abs, err := filepath.Abs(path); err == nil {
		path = abs
	}
	mu, _ := fileReservationLocks.LoadOrStore(path, &sync.Mutex{})
	return mu.(*sync.Mutex)
}

func reserveKey(store IdempotencyStore, scope, id, candidate string) (string, error) {
	if atomic, ok := store.(AtomicIdempotencyStore); ok {
		return atomic.Reserve(scope, id, candidate)
	}
	legacyReservationMu.Lock()
	defer legacyReservationMu.Unlock()
	if old, ok := store.Get(scope, id); ok && old != "" {
		return old, nil
	}
	store.Put(scope, id, candidate)
	if old, ok := store.Get(scope, id); !ok || old != candidate {
		return "", fmt.Errorf("the key could not be stored")
	}
	return candidate, nil
}
func (s *MemoryIdempotencyStore) Reserve(scope, id, candidate string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	slot := [2]string{scope, id}
	if old := s.keys[slot]; old != "" {
		return old, nil
	}
	s.keys[slot] = candidate
	return candidate, nil
}
func (s *FileIdempotencyStore) Reserve(scope, id, candidate string) (string, error) {
	fileMu := fileReservationLock(s.path)
	fileMu.Lock()
	defer fileMu.Unlock()
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := os.MkdirAll(filepath.Dir(s.path), 0700); err != nil {
		return "", err
	}
	lease := s.path + ".lock"
	if err := acquireLease(lease); err != nil {
		return "", err
	}
	// The outcome is decided; a lease left behind turns stale and is broken.
	defer func() { _ = os.Remove(lease) }()
	data, ok := s.read()
	if !ok {
		return "", fmt.Errorf("invalid idempotency store")
	}
	if old := data[scope][id]; old != "" {
		return old, nil
	}
	if data[scope] == nil {
		data[scope] = map[string]string{}
	}
	data[scope][id] = candidate
	if err := s.write(data); err != nil {
		return "", err
	}
	return candidate, nil
}

// leaseWait is how long Reserve waits for another writer's lease before
// refusing; a lease older than leaseStale is a crashed writer's and is broken.
const (
	leaseWait  = 2 * time.Second
	leaseStale = 30 * time.Second
)

// acquireLease takes the cross-process lease (a directory: mkdir is atomic).
// A busy lease is retried with backoff for up to leaseWait; one whose mtime is
// older than leaseStale was left by a writer that died and is removed. A lease
// still busy after the wait refuses the call.
func acquireLease(lease string) error {
	deadline := time.Now().Add(leaseWait)
	for delay := 5 * time.Millisecond; ; {
		err := os.Mkdir(lease, 0700)
		if err == nil {
			return nil
		}
		if !errors.Is(err, fs.ErrExist) {
			return err
		}
		// Retry at once when the lease is gone or a stale one was removed.
		info, err := os.Stat(lease)
		retryNow := errors.Is(err, fs.ErrNotExist) ||
			(err == nil && time.Since(info.ModTime()) > leaseStale && os.Remove(lease) == nil)
		left := time.Until(deadline)
		if left <= 0 {
			return fmt.Errorf("the idempotency store is locked by another writer (%s); try again", lease)
		}
		if !retryNow {
			time.Sleep(min(delay, left))
			delay = min(delay*2, 100*time.Millisecond)
		}
	}
}

// A permit is retained until persistence completes, including after caller
// cancellation. Waiting callers never enqueue an unbounded number of workers.
var reservationWorkers = make(chan struct{}, 16)

type reservationResult struct {
	key string
	err error
}

func reserveKeyContext(ctx context.Context, store IdempotencyStore, scope, id, candidate string) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	select {
	case reservationWorkers <- struct{}{}:
	case <-ctx.Done():
		return "", ctx.Err()
	}
	if err := ctx.Err(); err != nil {
		<-reservationWorkers
		return "", err
	}
	finished := make(chan reservationResult, 1)
	go func() {
		defer func() { <-reservationWorkers }()
		result := reservationResult{}
		func() {
			defer func() {
				if recover() != nil {
					result.err = errors.New("idempotency store worker panicked")
				}
			}()
			result.key, result.err = reserveKey(store, scope, id, candidate)
		}()
		finished <- result
	}()
	select {
	case result := <-finished:
		if err := ctx.Err(); err != nil {
			return "", err
		}
		return result.key, result.err
	case <-ctx.Done():
		return "", ctx.Err()
	}
}
