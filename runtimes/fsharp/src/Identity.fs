// SPDX-License-Identifier: Apache-2.0
/// The F# identity of the clients of generated F# SDKs: `User-Agent: ... (fsharp)` and
/// `X-Tungsten-Runtime: tungsten-fs/<version>`. Transport, semantics and the prose of envelopes
/// are the .NET runtime's (Tungsten.Runtime).
module Tungsten.Runtime.FSharp.Identity

open Tungsten.Runtime

/// The version of this layer, sent as `X-Tungsten-Runtime: tungsten-fs/<version>`.
[<Literal>]
let Version = "0.1.6"

/// The `(language)` token of `User-Agent`.
[<Literal>]
let Language = "fsharp"

/// The runtime id of `X-Tungsten-Runtime`.
[<Literal>]
let Runtime = "fs"

/// The identity a generated F# client hands to the .NET runtime.
let value: RuntimeIdentity = RuntimeIdentity(Language, Runtime, Version)

/// The engine of a generated F# client: the .NET runtime's `ClientCore` over a descriptor
/// document, identified as F#. Throws `ClientConfigException` when the options are invalid.
let core (document: SdkDescriptors) (options: ClientOptions option) : ClientCore =
    ClientCore(document, Option.toObj options, value)
