// SPDX-License-Identifier: Apache-2.0
namespace Tungsten.Runtime.FSharp

open System
open System.Threading

/// `option` interop with the .NET runtime, whose optional values are nulls and `Nullable<T>`.
/// Generated methods take `?options` and `?cancellationToken`; these turn them into the
/// runtime's arguments.
[<RequireQualifiedAccess>]
module Interop =
    /// `None` as null (an optional object argument of the runtime).
    let orNull (value: 'T option) : 'T when 'T: null = Option.toObj value

    /// A possibly-null object of the runtime as an option.
    let ofObj (value: 'T) : 'T option when 'T: null = Option.ofObj value

    /// `None` as an empty `Nullable<T>`.
    let toNullable (value: 'T option) : Nullable<'T> = Option.toNullable value

    /// A `Nullable<T>` of the runtime as an option.
    let ofNullable (value: Nullable<'T>) : 'T option = Option.ofNullable value

    /// The cancellation token of an optional argument: `CancellationToken.None` when absent.
    let token (value: CancellationToken option) : CancellationToken =
        defaultArg value CancellationToken.None
