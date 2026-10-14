// SPDX-License-Identifier: Apache-2.0
namespace Tungsten.Runtime.FSharp

open System.Collections.Generic
open System.Runtime.ExceptionServices
open System.Threading
open System.Threading.Tasks
open Tungsten.Runtime

/// `task { }`-friendly wrappers over the results of generated methods.
[<RequireQualifiedAccess>]
module TaskResult =
    /// The F# result of a call: the value, or the envelope of the failure.
    let toResult (call: Task<Result<'T>>) : Task<Result<'T, Diagnostic>> =
        task {
            let! r = call
            return RuntimeResult.toResult r
        }

    /// The value of a call; the task fails with `TungstenException` on a failure.
    let unwrap (call: Task<Result<'T>>) : Task<'T> =
        task {
            let! r = call
            return r.Unwrap()
        }

    /// The value of a call, if it succeeded.
    let toOption (call: Task<Result<'T>>) : Task<'T option> =
        task {
            let! r = call
            return RuntimeResult.toOption r
        }

    /// The value of a successful call mapped; a failure stays as it is.
    let map (f: 'T -> 'U) (call: Task<Result<'T>>) : Task<Result<'U, Diagnostic>> =
        task {
            let! r = call
            return RuntimeResult.toResult r |> Result.map f
        }

    /// The next call made with the value of a successful one; the first failure ends the chain.
    let bind (f: 'T -> Task<Result<'U>>) (call: Task<Result<'T>>) : Task<Result<'U, Diagnostic>> =
        task {
            let! r = call
            match RuntimeResult.toResult r with
            | Ok v ->
                let! next = f v
                return RuntimeResult.toResult next
            | Error d -> return Error d
        }

/// Helpers over `IAsyncEnumerable` (pages and streams) without a further dependency.
[<RequireQualifiedAccess>]
module AsyncEnumerable =
    /// Runs `f` on every element, in order, until `f` answers false or the source ends; the
    /// enumerator is disposed in every case.
    let private consume (f: 'T -> Task<bool>) (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<unit> =
        task {
            let e = source.GetAsyncEnumerator(cancellationToken)
            let mutable failure: ExceptionDispatchInfo option = None
            try
                let mutable go = true
                while go do
                    let! more = e.MoveNextAsync()
                    if more then
                        let! keep = f e.Current
                        go <- keep
                    else
                        go <- false
            with ex ->
                failure <- Some(ExceptionDispatchInfo.Capture ex)
            do! e.DisposeAsync()
            match failure with
            | Some info -> info.Throw()
            | None -> ()
        }

    /// Runs `f` on every element, in order; the enumerator is disposed in every case.
    let iter (f: 'T -> unit) (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<unit> =
        consume
            (fun x ->
                f x
                Task.FromResult true)
            source
            cancellationToken

    /// Awaits `f` on every element, in order; the enumerator is disposed in every case.
    let iterTask (f: 'T -> Task<unit>) (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<unit> =
        consume
            (fun x ->
                task {
                    do! f x
                    return true
                })
            source
            cancellationToken

    /// Every element, in order.
    let toList (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<'T list> =
        task {
            let items = List<'T>()
            do! iter items.Add source cancellationToken
            return List.ofSeq items
        }

    /// The first element for which `predicate` holds, then the enumeration stops.
    let tryFind (predicate: 'T -> bool) (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<'T option> =
        task {
            let mutable found = None
            do!
                consume
                    (fun x ->
                        if predicate x then
                            found <- Some x
                            Task.FromResult false
                        else
                            Task.FromResult true)
                    source
                    cancellationToken
            return found
        }

    /// The elements mapped lazily, one by one as they are enumerated.
    let map (f: 'T -> 'U) (source: IAsyncEnumerable<'T>) : IAsyncEnumerable<'U> =
        { new IAsyncEnumerable<'U> with
            member _.GetAsyncEnumerator(cancellationToken) =
                let e = source.GetAsyncEnumerator(cancellationToken)
                let current = ref Unchecked.defaultof<'U>
                { new IAsyncEnumerator<'U> with
                    member _.Current = current.Value

                    member _.MoveNextAsync() =
                        ValueTask<bool>(
                            task {
                                let! more = e.MoveNextAsync()
                                if more then current.Value <- f e.Current
                                return more
                            }
                        )

                    member _.DisposeAsync() = e.DisposeAsync() } }

    /// The elements of an F# sequence as an asynchronous sequence.
    let ofSeq (items: seq<'T>) : IAsyncEnumerable<'T> =
        { new IAsyncEnumerable<'T> with
            member _.GetAsyncEnumerator(cancellationToken) =
                let e = items.GetEnumerator()
                { new IAsyncEnumerator<'T> with
                    member _.Current = e.Current

                    member _.MoveNextAsync() =
                        cancellationToken.ThrowIfCancellationRequested()
                        ValueTask<bool>(e.MoveNext())

                    member _.DisposeAsync() =
                        e.Dispose()
                        ValueTask.CompletedTask } }

/// Helpers over the pages of paginated operations (`<Op>PagesAsync`).
[<RequireQualifiedAccess>]
module Pages =
    /// Every page as an F# result, lazily.
    let results (pages: IAsyncEnumerable<Result<Page<'T>>>) : IAsyncEnumerable<Result<Page<'T>, Diagnostic>> =
        AsyncEnumerable.map RuntimeResult.toResult pages

    /// The items of every page, in order, or the failure that ended the pages (the pages stop
    /// at the first failure).
    let items (pages: IAsyncEnumerable<Result<Page<'T>>>) (cancellationToken: CancellationToken) : Task<Result<'T list, Diagnostic>> =
        task {
            let collected = List<'T>()
            let! failure =
                AsyncEnumerable.tryFind
                    (fun (page: Result<Page<'T>>) ->
                        match RuntimeResult.toResult page with
                        | Ok p ->
                            collected.AddRange p.Items
                            false
                        | Error _ -> true)
                    pages
                    cancellationToken
            return
                match failure |> Option.map RuntimeResult.toResult with
                | Some(Error d) -> Error d
                | _ -> Ok(List.ofSeq collected)
        }

/// Helpers over server-sent event streams (`<Op>StreamAsync`).
[<RequireQualifiedAccess>]
module Stream =
    /// The value of a stream item as an F# result.
    let item (i: StreamItem<'T>) : Result<'T, Diagnostic> =
        match i with
        | :? StreamItem<'T>.Ok as ok -> Ok ok.Value
        | :? StreamItem<'T>.Err as err -> Error err.Error.Diagnostic
        | _ -> invalidOp "a StreamItem is Ok or Err"

    /// Every item as an F# result, lazily (the last one is the failure that ended the stream).
    let results (stream: EventStream<'T>) : IAsyncEnumerable<Result<'T, Diagnostic>> =
        AsyncEnumerable.map item (stream :> IAsyncEnumerable<StreamItem<'T>>)

    /// The values of every event, in order, or the failure that ended the stream with the values
    /// delivered before it. The collection is bounded as the .NET runtime's `CollectAsync` is
    /// (`ClientOptions.MaxCollectBytes`, `MaxCollectTime`).
    let values (stream: EventStream<'T>) (cancellationToken: CancellationToken) : Task<Result<'T list, Diagnostic * 'T list>> =
        task {
            let! folded = stream.CollectAsync(cancellationToken)
            let values = folded.Value |> Seq.map (fun (ok: StreamItem<'T>.Ok) -> ok.Value) |> List.ofSeq
            return
                match folded.Error with
                | null -> Ok values
                | error -> Error(error.Diagnostic, values)
        }

    /// The events folded into one value, or the failure with the value folded before it.
    let fold (folder: 'S -> 'T -> 'S) (state: 'S) (stream: EventStream<'T>) (cancellationToken: CancellationToken) : Task<Result<'S, Diagnostic * 'S>> =
        task {
            let! folded = stream.ReduceAsync(state, System.Func<'S, 'T, 'S>(folder), cancellationToken)
            return
                match folded.Error with
                | null -> Ok folded.Value
                | error -> Error(error.Diagnostic, folded.Value)
        }

    /// Ends the stream: no further event is delivered.
    let cancel (stream: EventStream<'T>) : unit = stream.Cancel()
