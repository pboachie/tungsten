// SPDX-License-Identifier: Apache-2.0
/// The JSON helpers generated F# records, unions and enums are written with: decoders of the
/// .NET runtime's JSON tree into F# values (options, lists, maps, `Patch`) through its `Decoder`
/// (strict for arguments, lenient for responses, with the path of a mismatch), and encoders
/// back. No reflection.
module Tungsten.Runtime.FSharp.Codec

open System
open System.Text.Json.Nodes
open Tungsten.Runtime

/// A decoder of one JSON value.
type Dec<'T> = JsonNode -> Decoder -> 'T

/// An encoder of one value.
type Enc<'T> = 'T -> JsonNode

let private func (dec: Dec<'T>) : Func<JsonNode, Decoder, 'T> = Func<JsonNode, Decoder, 'T>(fun n d -> dec n d)

// ---------------------------------------------------------------- decoders

/// A string.
let string: Dec<string> = fun n d -> Decoder.String(n, d)

/// A boolean.
let bool: Dec<bool> = fun n d -> Decoder.Boolean(n, d)

/// A 32-bit integer.
let int32: Dec<int> = fun n d -> Decoder.Int32(n, d)

/// A 64-bit integer.
let int64: Dec<int64> = fun n d -> Decoder.Int64(n, d)

/// A number.
let float: Dec<float> = fun n d -> Decoder.Double(n, d)

/// Base64 text as bytes.
let bytes: Dec<byte[]> = fun n d -> Decoder.Bytes(n, d)

/// The runtime's binary object (a body or a multipart file).
let binary: Dec<Binary> = fun n d -> Decoder.Binary(n, d)

/// Any JSON value, copied (JSON null is `null`).
let any: Dec<JsonNode> = fun n d -> Decoder.Any(n, d)

/// An array as an F# list.
let list (item: Dec<'T>) : Dec<'T list> =
    fun n d -> Decoder.List(n, d, func item) |> List.ofSeq

/// An object of values as an F# map.
let map (value: Dec<'T>) : Dec<Map<string, 'T>> =
    fun n d -> Decoder.Map(n, d, func value) |> Seq.map (fun kv -> kv.Key, kv.Value) |> Map.ofSeq

/// A value that may be null: `None` for JSON null.
let nullable (inner: Dec<'T>) : Dec<'T option> =
    fun n d -> if Json.IsNull n then None else Some(inner n d)

/// The value as an object, else a mismatch.
let object (d: Decoder) (node: JsonNode) : JsonObject = d.Object node

/// Refuses members other than `known` when the decoder is strict.
let known (d: Decoder) (o: JsonObject) (known: string[]) : unit = d.Known(o, known)

/// A required member.
let required (d: Decoder) (o: JsonObject) (name: string) (dec: Dec<'T>) : 'T = d.Required(o, name, func dec)

/// A required member that may be null: `None` for JSON null.
let requiredNullable (d: Decoder) (o: JsonObject) (name: string) (dec: Dec<'T>) : 'T option =
    d.Required(o, name, func (nullable dec))

/// An optional member: `None` when absent.
let optional (d: Decoder) (o: JsonObject) (name: string) (dec: Dec<'T>) : 'T option =
    match o.TryGetPropertyValue name with
    | true, v -> Some(d.At(name, v, func dec))
    | _ -> None

/// An optional member that may be null: absent, null or a value.
let patch (d: Decoder) (o: JsonObject) (name: string) (dec: Dec<'T>) : Patch<'T> =
    match o.TryGetPropertyValue name with
    | true, v when Json.IsNull v -> Patch.Null
    | true, v -> Patch.Value(d.At(name, v, func dec))
    | _ -> Patch.Absent

/// The members of an open record its type does not declare; `None` when there are none.
let extra (d: Decoder) (o: JsonObject) (known: string[]) : JsonObject option = d.Extra(o, known) |> Option.ofObj

/// The members of a record with typed additional members its type does not declare; `None`
/// when there are none.
let extraTyped (d: Decoder) (o: JsonObject) (known: string[]) (dec: Dec<'T>) : Map<string, 'T> option =
    d.ExtraTyped(o, func dec, known)
    |> Option.ofObj
    |> Option.map (fun m -> m |> Seq.map (fun kv -> kv.Key, kv.Value) |> Map.ofSeq)

/// The value of a discriminator member, else a mismatch at its path.
let tag (d: Decoder) (o: JsonObject) (name: string) : string = d.Tag(o, name)

/// The failure for a tag value no variant has.
let unknownTag (d: Decoder) (name: string) (value: string) (known: string[]) : exn =
    d.UnknownTag(name, value, known) :> exn

/// Tries the variants in order; the first that decodes (strictly, then leniently when the
/// decoder is lenient) wins.
let firstOf (d: Decoder) (node: JsonNode) (what: string) (variants: Dec<'T> list) : 'T =
    d.FirstOf(node, what, variants |> List.map func |> Array.ofList)

/// A known value of an enum, else (strict decoding) a failure listing the known values, or
/// (lenient decoding) the value kept by `unknown`.
let oneOf (d: Decoder) (node: JsonNode) (known: ('W * 'T) list) (wire: Dec<'W>) (encode: Enc<'W>) (unknown: 'W -> 'T) : 'T =
    let value = wire node d
    match known |> List.tryFind (fun (w, _) -> w = value) with
    | Some(_, v) -> v
    | None when d.Strict -> raise (d.NotOneOf(node, known |> List.map (fun (w, _) -> encode w)))
    | None -> unknown value

/// Whether a JSON value is this exact constant (the generator's literal).
let isLiteral (node: JsonNode) (literal: string) : bool = Json.Equal(node, Typed.Parse literal)

/// Whether a JSON value is of this kind.
let isKind (node: JsonNode) (kind: JsonKind) : bool = Json.KindOf node = kind

// ---------------------------------------------------------------- encoders

/// A string.
let encString: Enc<string> = fun v -> Encode.String v

/// A boolean.
let encBool: Enc<bool> = fun v -> Encode.Boolean v

/// A 32-bit integer.
let encInt32: Enc<int> = fun v -> Encode.Int32 v

/// A 64-bit integer.
let encInt64: Enc<int64> = fun v -> Encode.Int64 v

/// A number (integral values stay integers).
let encFloat: Enc<float> = fun v -> Encode.Double v

/// Bytes as base64 text.
let encBytes: Enc<byte[]> = fun v -> Encode.Bytes v

/// The runtime's binary object.
let encBinary: Enc<Binary> = fun v -> Encode.Binary v

/// Any JSON value, copied.
let encAny: Enc<JsonNode> = fun v -> Encode.Any v

/// A list as an array.
let encList (item: Enc<'T>) : Enc<'T list> =
    fun v -> Json.Array(v |> Seq.map item) :> JsonNode

/// A map as an object.
let encMap (value: Enc<'T>) : Enc<Map<string, 'T>> =
    fun v ->
        let o = JsonObject()
        for kv in v do
            Json.Put(o, kv.Key, value kv.Value)
        o :> JsonNode

/// A value that may be null: `None` as JSON null.
let encNullable (inner: Enc<'T>) : Enc<'T option> =
    fun v ->
        match v with
        | Some x -> inner x
        | None -> null

/// Sets a member.
let put (o: JsonObject) (name: string) (value: JsonNode) : unit = Encode.Put(o, name, value)

/// Sets a member when the value is present.
let putOption (o: JsonObject) (name: string) (enc: Enc<'T>) (value: 'T option) : unit =
    match value with
    | Some v -> Encode.Put(o, name, enc v)
    | None -> ()

/// Sets a member to null or a value when present.
let putPatch (o: JsonObject) (name: string) (enc: Enc<'T>) (value: Patch<'T>) : unit =
    match value with
    | Patch.Absent -> ()
    | Patch.Null -> Encode.Put(o, name, null)
    | Patch.Value v -> Encode.Put(o, name, enc v)

/// Sets the members of an open record its type does not declare.
let putExtra (o: JsonObject) (extra: JsonObject option) : unit =
    match extra with
    | Some e -> Encode.Extra(o, e)
    | None -> ()

/// Sets the typed additional members of a record.
let putExtraTyped (o: JsonObject) (enc: Enc<'T>) (extra: Map<string, 'T> option) : unit =
    match extra with
    | Some e ->
        for kv in e do
            Encode.Put(o, kv.Key, enc kv.Value)
    | None -> ()

/// Parses JSON text (the generator's literals and examples).
let parse (text: string) : JsonNode = Json.Parse text
