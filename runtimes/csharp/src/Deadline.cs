// SPDX-License-Identifier: Apache-2.0
using System;
using System.Diagnostics;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

public sealed partial class ClientCore
{
    /// <summary>Monotonic milliseconds, independent of the wall clock and of <see cref="ClientOptions.Now"/>.</summary>
    internal static double MonotonicMs() => Stopwatch.GetTimestamp() * 1000.0 / Stopwatch.Frequency;

    /// <summary>
    /// Fixes the total deadline of one logical call when a public entry point starts it. Nested
    /// work (pages, polls, verification, macro steps) inherits it through <c>with</c> copies and
    /// takes the earlier one, so it is never restarted or extended.
    /// </summary>
    internal CallOptions WithCallDeadline(CallOptions opts)
    {
        var total = opts.Deadline is TimeSpan d && d > TimeSpan.Zero ? d : deadline;
        if (total is not TimeSpan t || t <= TimeSpan.Zero)
        {
            return opts;
        }
        double at = MonotonicMs() + t.TotalMilliseconds;
        return opts with { CallDeadline = opts.CallDeadline is double inherited ? Math.Min(inherited, at) : at };
    }

    /// <summary>The time left before the call's deadline, or null without one.</summary>
    internal static TimeSpan? Remaining(CallOptions opts) =>
        opts.CallDeadline is double at ? TimeSpan.FromMilliseconds(Math.Max(0, at - MonotonicMs())) : null;

    internal static bool DeadlinePassed(CallOptions opts) => opts.CallDeadline is double at && MonotonicMs() >= at;

    internal static Failure DeadlineExpired(OperationDescriptor op, string during) =>
        new Diag(op.Id, Category.TransportFailed).Code("DEADLINE_EXCEEDED").Retry(Retryable.Never)
            .Remedy($"The operation deadline (Deadline) expired {during}, so the request was not sent. Call again with a larger Deadline.").Fail();

    /// <summary>Fails unsent once the call's deadline has passed.</summary>
    internal static void CheckDeadline(OperationDescriptor op, CallOptions opts, string during)
    {
        if (DeadlinePassed(opts))
        {
            throw DeadlineExpired(op, during);
        }
    }

    /// <summary>
    /// Waits for <paramref name="work"/> only until the call's deadline. The work is not
    /// cancelled: a shared token refresh keeps running for its other waiters, bounded by its own
    /// timeout. Returns false when the deadline came first.
    /// </summary>
    internal static async Task<(bool Done, T Value)> UntilDeadlineAsync<T>(Task<T> work, CallOptions opts)
    {
        if (Remaining(opts) is not TimeSpan left)
        {
            return (true, await work.ConfigureAwait(false));
        }
        if (left > TimeSpan.Zero)
        {
            try
            {
                return (true, await work.WaitAsync(left).ConfigureAwait(false));
            }
            catch (TimeoutException) when (!work.IsCompleted)
            {
            }
        }
        else if (work.IsCompleted)
        {
            return (true, await work.ConfigureAwait(false));
        }
        // Observe a later fault so it never surfaces as an unobserved task exception.
        _ = work.ContinueWith(static t => _ = t.Exception, TaskScheduler.Default);
        return (false, default!);
    }
}
