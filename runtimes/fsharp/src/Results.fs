// SPDX-License-Identifier: Apache-2.0
namespace Tungsten.Runtime.FSharp

open Tungsten.Runtime

/// Helpers over the error envelope (`Tungsten.Runtime.Diagnostic`).
[<RequireQualifiedAccess>]
module Diagnostic =
    /// The category as the contract spells it (`VALIDATION_FAILED`, ...).
    let category (d: Diagnostic) : string = Wire.Of d.Category

    /// Whether and how the call may be repeated, as the contract spells it (`never`, ...).
    let retryable (d: Diagnostic) : string = Wire.Of d.Retryable

    /// The call may be repeated as it is (after a delay, or with the same idempotency key).
    let isRetryable (d: Diagnostic) : bool =
        match d.Retryable with
        | Retryable.AfterDelay
        | Retryable.SameKeyOnly -> true
        | _ -> false

    /// What to do about the failure.
    let remediation (d: Diagnostic) : string = d.Remediation

    /// The envelope as the JSON object every tungsten runtime hands out.
    let toJson (d: Diagnostic) : System.Text.Json.Nodes.JsonObject = d.ToJson()

/// The .NET runtime's `Result<T>` as F# values.
[<RequireQualifiedAccess>]
module RuntimeResult =
    /// The value, or the envelope of the failure.
    let toResult (r: Result<'T>) : Result<'T, Diagnostic> =
        match r with
        | :? Result<'T>.Ok as ok -> Ok ok.Value
        | :? Result<'T>.Err as err -> Error err.Diagnostic
        | _ -> invalidOp "a Result is Ok or Err"

    /// The value, or the failure with what the call already produced (`TungstenError.Partial`).
    let toResultWithError (r: Result<'T>) : Result<'T, TungstenError> =
        match r with
        | :? Result<'T>.Ok as ok -> Ok ok.Value
        | :? Result<'T>.Err as err -> Error err.Error
        | _ -> invalidOp "a Result is Ok or Err"

    /// The value; raises `TungstenException` on a failure.
    let unwrap (r: Result<'T>) : 'T = r.Unwrap()

    /// The value, if the call succeeded.
    let toOption (r: Result<'T>) : 'T option =
        match r with
        | :? Result<'T>.Ok as ok -> Some ok.Value
        | _ -> None

    /// The HTTP answer of a success.
    let meta (r: Result<'T>) : ResponseMeta option =
        match r with
        | :? Result<'T>.Ok as ok -> Some ok.Meta
        | _ -> None

/// Active patterns over the .NET runtime's results.
[<AutoOpen>]
module ResultPatterns =
    /// `Success value` or `Failure diagnostic`.
    let (|Success|Failure|) (r: Result<'T>) : Choice<'T, Diagnostic> =
        match r with
        | :? Result<'T>.Ok as ok -> Choice1Of2 ok.Value
        | :? Result<'T>.Err as err -> Choice2Of2 err.Diagnostic
        | _ -> invalidOp "a Result is Ok or Err"
