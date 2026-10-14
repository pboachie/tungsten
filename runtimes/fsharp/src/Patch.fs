// SPDX-License-Identifier: Apache-2.0
namespace Tungsten.Runtime.FSharp

/// An optional member that may also be null: absent (not sent), an explicit null, or a value.
/// It is the F# form of the .NET runtime's `Tungsten.Runtime.Patch<T>`.
[<RequireQualifiedAccess>]
type Patch<'T> =
    /// The member is absent.
    | Absent
    /// The member is an explicit JSON null.
    | Null
    /// The member holds a value.
    | Value of 'T

/// Functions over `Patch<'T>`.
[<RequireQualifiedAccess>]
module Patch =
    /// The F# form of a .NET runtime patch.
    let ofRuntime (p: Tungsten.Runtime.Patch<'T>) : Patch<'T> =
        if not p.IsPresent then Patch.Absent
        elif p.IsNull then Patch.Null
        else Patch.Value p.Value

    /// The .NET runtime form of a patch.
    let toRuntime (p: Patch<'T>) : Tungsten.Runtime.Patch<'T> =
        match p with
        | Patch.Absent -> Tungsten.Runtime.Patch<'T>.Absent
        | Patch.Null -> Tungsten.Runtime.Patch<'T>.Null
        | Patch.Value v -> Tungsten.Runtime.Patch<'T>.Of v

    /// `Some v` as a value, `None` as absent.
    let ofOption (o: 'T option) : Patch<'T> =
        match o with
        | Some v -> Patch.Value v
        | None -> Patch.Absent

    /// The value, if the member holds one.
    let toOption (p: Patch<'T>) : 'T option =
        match p with
        | Patch.Value v -> Some v
        | Patch.Absent
        | Patch.Null -> None

    /// The member is present (null or a value).
    let isPresent (p: Patch<'T>) : bool =
        match p with
        | Patch.Absent -> false
        | Patch.Null
        | Patch.Value _ -> true

    /// The value mapped; absent and null stay as they are.
    let map (f: 'T -> 'U) (p: Patch<'T>) : Patch<'U> =
        match p with
        | Patch.Absent -> Patch.Absent
        | Patch.Null -> Patch.Null
        | Patch.Value v -> Patch.Value(f v)

    /// The value, else `fallback` (for absent and null).
    let defaultValue (fallback: 'T) (p: Patch<'T>) : 'T =
        match p with
        | Patch.Value v -> v
        | Patch.Absent
        | Patch.Null -> fallback
