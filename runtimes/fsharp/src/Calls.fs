// SPDX-License-Identifier: Apache-2.0
namespace Tungsten.Runtime.FSharp

open System.Collections.Generic
open System.Text.Json.Nodes
open System.Threading.Tasks
open Tungsten.Runtime

/// The typed calls the methods of generated F# resources make: the .NET runtime's `Typed`
/// entry points with F# decoders (`Codec.Dec`) and the optional arguments of a generated method
/// (`?options`, `?cancellationToken`) as they arrive. Validation, transport, retries,
/// idempotency, confirmation and classification are the .NET runtime's.
[<RequireQualifiedAccess>]
module Call =
    let private func (dec: Codec.Dec<'T>) : System.Func<JsonNode, Decoder, 'T> =
        System.Func<JsonNode, Decoder, 'T>(fun n d -> dec n d)

    /// Calls an operation with an encoded request and decodes the body (leniently).
    let typed
        (core: ClientCore)
        (op: OperationDescriptor)
        (args: JsonObject)
        (dec: Codec.Dec<'T>)
        (options: CallOptions option)
        (cancellationToken: System.Threading.CancellationToken option)
        : Task<Result<'T>> =
        Typed.CallAsync(core, op, args, func dec, Interop.orNull options, Interop.token cancellationToken)

    /// Calls an operation that answers without a body.
    let noContent
        (core: ClientCore)
        (op: OperationDescriptor)
        (args: JsonObject)
        (options: CallOptions option)
        (cancellationToken: System.Threading.CancellationToken option)
        : Task<Result<NoContent>> =
        Typed.CallNoContentAsync(core, op, args, Interop.orNull options, Interop.token cancellationToken)

    /// What a call would send, without sending it.
    let preview
        (core: ClientCore)
        (op: OperationDescriptor)
        (args: JsonObject)
        (options: CallOptions option)
        (cancellationToken: System.Threading.CancellationToken option)
        : Task<Result<PreviewResult>> =
        core.PreviewAsync(op, args, Interop.orNull options, Interop.token cancellationToken)

    /// The pages of a paginated operation with typed items; an item that does not decode ends
    /// the pages with UNEXPECTED_RESPONSE.
    let pages
        (core: ClientCore)
        (op: OperationDescriptor)
        (args: JsonObject)
        (dec: Codec.Dec<'T>)
        (options: CallOptions option)
        (cancellationToken: System.Threading.CancellationToken option)
        : IAsyncEnumerable<Result<Page<'T>>> =
        Typed.PagesAsync(core, op, args, func dec, Interop.orNull options, Interop.token cancellationToken)

    /// The typed event stream of an operation; an event that does not decode ends the stream
    /// with UNEXPECTED_RESPONSE.
    let stream
        (core: ClientCore)
        (op: OperationDescriptor)
        (args: JsonObject)
        (dec: Codec.Dec<'T>)
        (options: CallOptions option)
        : EventStream<'T> =
        Typed.Stream(core, op, args, func dec, Interop.orNull options)

    /// Runs a macro of the descriptor document; the value is the macro's output as JSON.
    let macro
        (core: ClientCore)
        (macro: MacroDescriptor)
        (input: JsonObject)
        (options: CallOptions option)
        (cancellationToken: System.Threading.CancellationToken option)
        : Task<Result<JsonNode>> =
        core.RunMacroAsync(macro, input, Interop.orNull options, Interop.token cancellationToken)

    /// What a macro would send, without sending it.
    let previewMacro
        (core: ClientCore)
        (macro: MacroDescriptor)
        (input: JsonObject)
        (options: CallOptions option)
        (cancellationToken: System.Threading.CancellationToken option)
        : Task<Result<PreviewResult>> =
        core.PreviewMacroAsync(macro, input, Interop.orNull options, Interop.token cancellationToken)
