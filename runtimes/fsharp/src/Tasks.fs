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

    /// The value of a successful call mapped; a failure stays as it is.
    let map (f: 'T -> 'U) (call: Task<Result<'T>>) : Task<Result<'U, Diagnostic>> =
        task {
            let! r = call
            return RuntimeResult.toResult r |> Result.map f
        }

/// Helpers over `IAsyncEnumerable` (pages) without a further dependency.
[<RequireQualifiedAccess>]
module AsyncEnumerable =
    /// Runs `f` on every element, in order; the enumerator is disposed in every case.
    let iter (f: 'T -> unit) (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<unit> =
        task {
            let e = source.GetAsyncEnumerator(cancellationToken)
            let mutable failure: ExceptionDispatchInfo option = None
            try
                let mutable go = true
                while go do
                    let! more = e.MoveNextAsync()
                    if more then f e.Current else go <- false
            with ex ->
                failure <- Some(ExceptionDispatchInfo.Capture ex)
            do! e.DisposeAsync()
            match failure with
            | Some info -> info.Throw()
            | None -> ()
        }

    /// Every element, in order.
    let toList (source: IAsyncEnumerable<'T>) (cancellationToken: CancellationToken) : Task<'T list> =
        task {
            let items = List<'T>()
            do! iter items.Add source cancellationToken
            return List.ofSeq items
        }

/// Helpers over the pages of paginated operations.
[<RequireQualifiedAccess>]
module Pages =
    /// The items of every page, in order, or the failure that ended the pages.
    let items (pages: IAsyncEnumerable<Result<Page<'T>>>) (cancellationToken: CancellationToken) : Task<Result<'T list, Diagnostic>> =
        task {
            let! all = AsyncEnumerable.toList pages cancellationToken
            let collected = List<'T>()
            let mutable failure = None
            for page in all do
                if failure.IsNone then
                    match RuntimeResult.toResult page with
                    | Ok p -> collected.AddRange p.Items
                    | Error d -> failure <- Some d
            return
                match failure with
                | Some d -> Error d
                | None -> Ok(List.ofSeq collected)
        }

/// Helpers over server-sent event streams.
[<RequireQualifiedAccess>]
module Stream =
    /// The values of every event, in order, or the failure that ended the stream (with the values
    /// delivered before it).
    let values (stream: EventStream<'T>) (cancellationToken: CancellationToken) : Task<Result<'T list, Diagnostic * 'T list>> =
        task {
            let! items = AsyncEnumerable.toList (stream :> IAsyncEnumerable<StreamItem<'T>>) cancellationToken
            let values = List<'T>()
            let mutable failure = None
            for item in items do
                match item with
                | :? StreamItem<'T>.Ok as ok when failure.IsNone -> values.Add ok.Value
                | :? StreamItem<'T>.Err as err when failure.IsNone -> failure <- Some err.Error.Diagnostic
                | _ -> ()
            return
                match failure with
                | Some d -> Error(d, List.ofSeq values)
                | None -> Ok(List.ofSeq values)
        }

    /// Ends the stream: no further event is delivered.
    let cancel (stream: EventStream<'T>) : unit = stream.Cancel()
