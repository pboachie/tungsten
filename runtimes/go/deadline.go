// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// errCallDeadline is the cause of a context ended by the operation deadline.
var errCallDeadline = errors.New("tungsten: the operation deadline expired")

// callDeadlineKey carries the absolute deadline of one logical call. Nested
// work (pages, polls, verification, macro steps) inherits it and can only
// shorten it, never restart it.
type callDeadlineKey struct{}

// deadlineFor is the total deadline of a call: the call's own, else the
// client's; zero means none.
func (c *ClientCore) deadlineFor(opts CallOptions) time.Duration {
	if opts.Deadline > 0 {
		return opts.Deadline
	}
	if c.deadline > 0 {
		return c.deadline
	}
	return 0
}

// deadlineAt is the absolute deadline of a call started now, or the zero
// time without one. time.Now carries a monotonic reading, so wall-clock
// changes do not move it.
func (c *ClientCore) deadlineAt(opts CallOptions) time.Time {
	if d := c.deadlineFor(opts); d > 0 {
		return time.Now().Add(d)
	}
	return time.Time{}
}

// withCallDeadline bounds ctx by the call's total deadline, if any.
func (c *ClientCore) withCallDeadline(ctx context.Context, opts CallOptions) (context.Context, context.CancelFunc) {
	return boundBy(ctx, c.deadlineAt(opts))
}

// boundBy records at as the call deadline (the earlier of at and an
// inherited one) and ends ctx then. The zero time leaves ctx as it is.
func boundBy(ctx context.Context, at time.Time) (context.Context, context.CancelFunc) {
	if at.IsZero() {
		return ctx, func() {}
	}
	if inherited, ok := callDeadline(ctx); ok && inherited.Before(at) {
		at = inherited
	}
	return context.WithDeadlineCause(context.WithValue(ctx, callDeadlineKey{}, at), at, errCallDeadline)
}

// withConnectDeadline records at as the call deadline without ending ctx:
// a stream's deadline bounds connecting, never the open stream, whose body
// lives on ctx.
func withConnectDeadline(ctx context.Context, at time.Time) context.Context {
	if at.IsZero() {
		return ctx
	}
	if inherited, ok := callDeadline(ctx); ok && inherited.Before(at) {
		at = inherited
	}
	return context.WithValue(ctx, callDeadlineKey{}, at)
}

// callDeadline is the call deadline ctx carries.
func callDeadline(ctx context.Context) (time.Time, bool) {
	at, ok := ctx.Value(callDeadlineKey{}).(time.Time)
	return at, ok
}

// deadlineExpired reports whether the call deadline ctx carries has passed.
func deadlineExpired(ctx context.Context) bool {
	at, ok := callDeadline(ctx)
	return ok && !time.Now().Before(at)
}

// deadlineFailure is the error of a call whose deadline expired before its
// request was sent.
func deadlineFailure(op, during string) *Error {
	return newDiag(op, TransportFailed).code(strPtr("DEADLINE_EXCEEDED")).retry(RetryNever).
		remedy(fmt.Sprintf("The operation deadline (Deadline) expired %s, so the request was not sent. Call again with a larger Deadline.", during)).err()
}
