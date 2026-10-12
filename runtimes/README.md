# Runtime polling and OAuth refresh contracts

Positive polling budgets use one monotonic deadline covering preparation,
authentication, transport and response bodies, retries and sleeps. A response
observed after that deadline cannot satisfy the poll, even when its predicate
matches. If a read has a completed observation, expiry returns that observation
with `timedOut` / `timed_out` / `TimedOut` set. Expiry before any observation
returns a transport diagnostic. Sent or possibly sent mutations retain an
unknown-outcome diagnostic; a completed earlier observation cannot establish
what a later mutation did. A zero budget deliberately performs exactly one
observation without a polling deadline; its ordinary call timeout still applies.
Caller cancellation continues to apply.

Go client-credentials refreshes belong to the shared per-scheme flight, with
an ordinary token-attempt timeout. Cancelling a waiter leaves that exchange
available to remaining callers. Rust uses a per-scheme asynchronous gate:
cancelling its refresh owner releases ownership and another waiter retries;
waiters are not failed by that cancellation. Network exchanges hold no shared
cache-map lock. Successful cache entries and failures are visible before a
flight is released, so later callers can reuse tokens or recover after failure.

Python synchronous polling bounds each effect with a deadline and at most
16 background workers. An effect already in progress can finish after its
caller returns, but cannot resume the abandoned call flow or send subsequent
requests. Python asynchronous effects are cancelled at the same deadline;
shared OAuth refresh ownership remains independent of waiter cancellation.
