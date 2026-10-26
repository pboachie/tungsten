// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json.Nodes;

namespace Tungsten.Runtime;

/// <summary>One parameter of an operation.</summary>
public sealed record ParamDescriptor
{
    /// <summary>The key in the arguments object (the arguments layout).</summary>
    public string Key { get; init; } = "";
    /// <summary>The SDK's argument name; informational for supplied parameters.</summary>
    public string Name { get; init; } = "";
    public string Wire { get; init; } = "";
    /// <summary><c>path</c>, <c>query</c>, <c>header</c> or <c>cookie</c>.</summary>
    public string In { get; init; } = "query";
    public bool Required { get; init; }
    public string Style { get; init; } = "form";
    public bool Explode { get; init; }
    /// <summary><c>plain</c>, <c>dry_run</c>, <c>idempotency_key</c>, <c>origin</c>, <c>auth</c> or <c>constant</c>.</summary>
    public string Role { get; init; } = "plain";
    /// <summary>The header value of a constant parameter.</summary>
    public string? Constant { get; init; }
    public bool Sensitive { get; init; }

    /// <summary>Whether the caller passes it as an argument (plain and dry-run parameters).</summary>
    public bool IsArgument => Role == "plain" || Role == "dry_run";
}

/// <summary>One field of a merged body: the arguments key, the SDK name and the wire name.</summary>
public sealed record MergedField(string Arg, string Name, string Wire);

/// <summary>The request body: the whole body is the argument <see cref="Arg"/>, or a JSON object of the merged <see cref="Fields"/>.</summary>
public sealed record BodyDescriptor
{
    public string MediaType { get; init; } = "application/json";
    /// <summary><c>json</c>, <c>form</c>, <c>multipart</c>, <c>bytes</c> or <c>text</c>.</summary>
    public string Encoding { get; init; } = "json";
    public bool Required { get; init; }
    public IReadOnlyList<MergedField> Fields { get; init; } = System.Array.Empty<MergedField>();
    /// <summary>The arguments key of a whole-body argument; null for a merged body.</summary>
    public string? Arg { get; init; }
    public string? ArgName { get; init; }
}

/// <summary>One declared response.</summary>
public sealed record ResponseDescriptor
{
    /// <summary><c>exact</c>, <c>class</c> (Code is the hundreds digit) or <c>default</c>.</summary>
    public string StatusKind { get; init; } = "default";
    public int Code { get; init; }
    /// <summary><c>success</c>, <c>error</c> or <c>ambiguous</c>.</summary>
    public string Kind { get; init; } = "success";
    public string? MediaType { get; init; }
}

/// <summary>How an operation pages; parameters are arguments keys.</summary>
public sealed record Pagination
{
    /// <summary><c>cursor</c>, <c>offset</c>, <c>page</c> or <c>link_header</c>.</summary>
    public string Style { get; init; } = "cursor";
    public string? RequestParam { get; init; }
    public string? ResponseField { get; init; }
    public string ItemsField { get; init; } = "";
    public string? PageSizeParam { get; init; }
    public string? HasMoreField { get; init; }
    public string? CursorItemField { get; init; }
    public string? OffsetParam { get; init; }
    public string? LimitParam { get; init; }
    public string? PageParam { get; init; }
    public string? SizeParam { get; init; }
}

/// <summary>A manifest remediation entry for an API error code.</summary>
public sealed record RemediationEntry
{
    public Category? Category { get; init; }
    public string? Text { get; init; }
    public Retryable? Retryable { get; init; }
    public string? NextAction { get; init; }
}

/// <summary>A verification hook.</summary>
public sealed record VerifyDescriptor
{
    public string Operation { get; init; } = "";
    public JsonNode? Args { get; init; }
    public JsonNode? Expect { get; init; }
    public JsonNode? Terminal { get; init; }
    public long? PollIntervalMs { get; init; }
    public long? PollBudgetMs { get; init; }
}

/// <summary>An operation's idempotency policy.</summary>
public sealed record IdempotencyMeta
{
    public IdempotencyKind Policy { get; init; }
    public string? Header { get; init; }
    public string? Format { get; init; }
    public bool PersistRequired { get; init; }
    public string? Note { get; init; }
}

/// <summary>How a preview works: <c>local</c>, <c>header</c>, <c>endpoint</c> or <c>none</c>.</summary>
public sealed record PreviewMode
{
    public string Mode { get; init; } = "local";
    public string? Header { get; init; }
    public string? Value { get; init; }
    public string? Operation { get; init; }
}

/// <summary>The confirmation text of an operation.</summary>
public sealed record ConfirmationMeta
{
    public IReadOnlyList<string> SummaryFields { get; init; } = System.Array.Empty<string>();
    public string? Message { get; init; }
}

/// <summary>The agent metadata of an operation.</summary>
public sealed record AgentMeta
{
    public Safety Safety { get; init; }
    public IdempotencyMeta Idempotency { get; init; } = new();
    public PreviewMode Preview { get; init; } = new();
    public ConfirmationMeta? Confirmation { get; init; }
    public VerifyDescriptor? Verify { get; init; }
    public IReadOnlyDictionary<string, RemediationEntry> Remediation { get; init; } = new Dictionary<string, RemediationEntry>();
    public string? RemediationNote { get; init; }
    public IReadOnlyList<string> SensitiveResponseFields { get; init; } = System.Array.Empty<string>();
    /// <summary>Dotted argument paths (arguments keys).</summary>
    public IReadOnlyList<string> SensitiveRequestFields { get; init; } = System.Array.Empty<string>();
    public bool ShownOnce { get; init; }
    /// <summary>The review policy of an operation imported from a collection or capture, or null.</summary>
    public ImportPolicy? ImportPolicy { get; init; }
}

/// <summary>
/// The review policy of an operation imported from a Postman collection, cURL
/// command, HTTP request file or HAR capture. A reviewed operation may be sent
/// only to one of its allowed origins; an unreviewed one carries an effective
/// safety tier that requires preview and confirmation.
/// </summary>
public sealed record ImportPolicy
{
    /// <summary>The origins (scheme, host, port) the review covers.</summary>
    public IReadOnlyList<string> AllowedOrigins { get; init; } = System.Array.Empty<string>();
    /// <summary>The tier inferred from the HTTP method.</summary>
    public Safety InferredSafety { get; init; } = Safety.Irreversible;
    /// <summary>Whether a person reviewed the operation.</summary>
    public bool Reviewed { get; init; }
    /// <summary>The kind of input the operation came from.</summary>
    public string Source { get; init; } = "";
}

/// <summary>A runtime gate: the operation is mounted only when EnvVar is on.</summary>
public sealed record Gate(string EnvVar, int DisabledStatus);

/// <summary>Puts an rpc-unflattened operation back on the wire.</summary>
public sealed record RpcDescriptor
{
    public string Field { get; init; } = "";
    public string Value { get; init; } = "";
    public string ParamsField { get; init; } = "";
    public JsonObject? Constants { get; init; }
}

/// <summary>The server-sent events of an operation.</summary>
public sealed record StreamDescriptor
{
    /// <summary>Validates each event's decoded data; null accepts any JSON.</summary>
    public Schema? Event { get; init; }
    /// <summary>A data value that ends the stream without being an event.</summary>
    public string? Done { get; init; }
    /// <summary>The wire name of the boolean body field that selects the stream.</summary>
    public string? Flag { get; init; }
}

/// <summary>Everything the runtime needs to call one operation.</summary>
public sealed record OperationDescriptor
{
    public string Id { get; init; } = "";
    public string Method { get; init; } = "GET";
    /// <summary>The path template as in the spec; placeholders use wire names.</summary>
    public string Path { get; init; } = "";
    /// <summary>Arguments first, then the supplied parameters.</summary>
    public IReadOnlyList<ParamDescriptor> Params { get; init; } = System.Array.Empty<ParamDescriptor>();
    public BodyDescriptor? Body { get; init; }
    public IReadOnlyList<ResponseDescriptor> Responses { get; init; } = System.Array.Empty<ResponseDescriptor>();
    /// <summary>An OR of AND-sets of scheme names; empty means no auth.</summary>
    public IReadOnlyList<IReadOnlyList<string>> Security { get; init; } = System.Array.Empty<IReadOnlyList<string>>();
    public Pagination? Pagination { get; init; }
    public RpcDescriptor? Rpc { get; init; }
    public string? ErrorCodeField { get; init; }
    public Gate? Gate { get; init; }
    public AgentMeta Agent { get; init; } = new();
    /// <summary>Validates the arguments object before any network call.</summary>
    public Schema? Request { get; init; }
    /// <summary>Validates a success body.</summary>
    public Schema? Response { get; init; }
    /// <summary>Validates each item of a page.</summary>
    public Schema? PageItem { get; init; }
    public StreamDescriptor? Stream { get; init; }
    public string? Summary { get; init; }

    internal IEnumerable<ParamDescriptor> Arguments => Params.Where(p => p.IsArgument);

    /// <summary>The arguments key of a parameter or field given by key or wire name.</summary>
    public string ArgName(string key)
    {
        foreach (var p in Params)
        {
            if (p.Key == key)
            {
                return p.Key;
            }
        }
        foreach (var p in Params)
        {
            if (p.Wire == key)
            {
                return p.Key;
            }
        }
        return key;
    }
}

/// <summary>One part of a composite auth profile.</summary>
public sealed record CompositePart
{
    /// <summary><c>cookie</c>, <c>header</c> or <c>bearer</c>.</summary>
    public string Kind { get; init; } = "";
    public string? Name { get; init; }
    public string? EqualsCookie { get; init; }
    public string? FromConfig { get; init; }
    public bool MutationOnly { get; init; }
    public string? Prefix { get; init; }
}

/// <summary>The authorizationCode flow of an OAuth2 scheme; RefreshUrl falls back to TokenUrl.</summary>
public sealed record AuthorizationCodeFlow
{
    public string? AuthorizationUrl { get; init; }
    public string? TokenUrl { get; init; }
    public string? RefreshUrl { get; init; }
    public IReadOnlyList<string> Scopes { get; init; } = System.Array.Empty<string>();
}

/// <summary>One auth scheme: <c>api_key</c>, <c>http_bearer</c>, <c>http_basic</c>, <c>oauth2</c> or <c>composite</c>.</summary>
public sealed record AuthScheme
{
    public string Kind { get; init; } = "";
    public string Name { get; init; } = "";
    public string In { get; init; } = "header";
    public string Wire { get; init; } = "";
    public string? Prefix { get; init; }
    public string? TokenUrl { get; init; }
    public IReadOnlyList<string> Scopes { get; init; } = System.Array.Empty<string>();
    public AuthorizationCodeFlow? AuthorizationCode { get; init; }
    public IReadOnlyList<string> Satisfies { get; init; } = System.Array.Empty<string>();
    public IReadOnlyList<CompositePart> Parts { get; init; } = System.Array.Empty<CompositePart>();
}

/// <summary>Maps an error answer without a JSON body; Media is a media type, or <c>none</c> for a bare status.</summary>
public sealed record NonJsonError
{
    public int Status { get; init; }
    public string Media { get; init; } = "none";
    public Category Category { get; init; }
    public Retryable Retryable { get; init; }
    public string? Text { get; init; }
}

/// <summary>The retry defaults by tier.</summary>
public sealed record TierRetries(RetryOptions ReadOnly, RetryOptions Mutating);

/// <summary>The API.</summary>
public sealed record ApiDescriptor
{
    public string Redirects { get; init; } = "same_origin_read";
    public string Name { get; init; } = "api";
    public string Version { get; init; } = "0";
    public string TungstenVersion { get; init; } = "";
    public IReadOnlyList<string> Servers { get; init; } = System.Array.Empty<string>();
    public IReadOnlyList<AuthScheme> Auth { get; init; } = System.Array.Empty<AuthScheme>();
    public IReadOnlyDictionary<string, RemediationEntry> ErrorCodes { get; init; } = new Dictionary<string, RemediationEntry>();
    public IReadOnlyList<int> AmbiguousStatuses { get; init; } = System.Array.Empty<int>();
    public IReadOnlyList<NonJsonError> NonJson { get; init; } = System.Array.Empty<NonJsonError>();
    public IReadOnlyDictionary<string, string> Gates { get; init; } = new Dictionary<string, string>();
    public TierRetries? Retries { get; init; }
}

/// <summary>One macro step in the canonical form.</summary>
public sealed record MacroStep
{
    /// <summary><c>call</c>, <c>poll</c> or <c>paginate</c>.</summary>
    public string Kind { get; init; } = "call";
    public string Operation { get; init; } = "";
    public JsonNode? Args { get; init; }
    public string? As { get; init; }
    public JsonNode? Until { get; init; }
    public long? IntervalMs { get; init; }
    public JsonNode? BudgetMs { get; init; }
    public long? MaxPages { get; init; }
}

/// <summary>One input field of a macro.</summary>
public sealed record MacroInputField(string Key, string Name, bool Required);

/// <summary>The input of a macro: the arguments of <see cref="Extends"/> plus the added fields.</summary>
public sealed record MacroInput
{
    public string? Extends { get; init; }
    /// <summary>Added fields as JSON Schemas; a <c>default</c> applies when the input omits the field.</summary>
    public JsonObject Add { get; init; } = new();
    public IReadOnlyList<MacroInputField> Fields { get; init; } = System.Array.Empty<MacroInputField>();
}

/// <summary>A compiled macro.</summary>
public sealed record MacroDescriptor
{
    public string Name { get; init; } = "";
    public string Summary { get; init; } = "";
    public Safety Safety { get; init; }
    public IReadOnlyList<MacroStep> Steps { get; init; } = System.Array.Empty<MacroStep>();
    public JsonNode? Output { get; init; }
    public MacroInput Input { get; init; } = new();
    public IReadOnlyList<string> SensitiveResponseFields { get; init; } = System.Array.Empty<string>();
    public bool ShownOnce { get; init; }
    public string? Cluster { get; init; }
}

/// <summary>
/// The SDK descriptor document v1 (<c>tungsten-sdk-descriptors/1</c>): the API, every callable
/// operation by id and every macro by name. A generated SDK embeds it as the resource
/// <c>tungsten-descriptors.json</c> and loads it once.
/// </summary>
public sealed class SdkDescriptors
{
    /// <summary>The format this runtime reads.</summary>
    public const string Format = "tungsten-sdk-descriptors/1";

    private SdkDescriptors(string target, ApiDescriptor api, IReadOnlyDictionary<string, OperationDescriptor> operations, IReadOnlyDictionary<string, MacroDescriptor> macros)
    {
        Target = target;
        Api = api;
        Operations = operations;
        Macros = macros;
    }

    /// <summary>The target the names are spelled for.</summary>
    public string Target { get; }

    public ApiDescriptor Api { get; }

    /// <summary>Every callable operation by id.</summary>
    public IReadOnlyDictionary<string, OperationDescriptor> Operations { get; }

    /// <summary>Every macro by name.</summary>
    public IReadOnlyDictionary<string, MacroDescriptor> Macros { get; }

    /// <summary>The operation with this id; throws when the document has none.</summary>
    public OperationDescriptor Operation(string id) =>
        Operations.TryGetValue(id, out var op) ? op : throw new ArgumentException("the descriptor document has no operation " + id);

    /// <summary>The macro with this name; throws when the document has none.</summary>
    public MacroDescriptor Macro(string name) =>
        Macros.TryGetValue(name, out var m) ? m : throw new ArgumentException("the descriptor document has no macro " + name);

    /// <summary>Reads a document from a stream (an embedded resource).</summary>
    public static SdkDescriptors Load(Stream? stream)
    {
        if (stream is null)
        {
            throw new ArgumentException("the descriptor document is missing");
        }
        using (stream)
        {
            using var memory = new MemoryStream();
            stream.CopyTo(memory);
            return Parse(Encoding.UTF8.GetString(memory.ToArray()));
        }
    }

    /// <summary>Reads a document from its text.</summary>
    public static SdkDescriptors Parse(string text)
    {
        JsonNode? doc;
        try
        {
            doc = Json.Parse(text);
        }
        catch (JsonParseException e)
        {
            throw new ArgumentException("the descriptor document is not JSON: " + e.Message, e);
        }
        if (doc is not JsonObject d || Json.AsString(d["format"]) != Format)
        {
            throw new ArgumentException("not a " + Format + " document");
        }
        var defs = Schema.ReadDefs(d["defs"]);
        var api = ReadApi(Obj(d["api"]));
        var ops = new SortedDictionary<string, OperationDescriptor>(StringComparer.Ordinal);
        foreach (var kv in Obj(d["operations"]))
        {
            ops[kv.Key] = ReadOperation(Obj(kv.Value), defs);
        }
        var macros = new SortedDictionary<string, MacroDescriptor>(StringComparer.Ordinal);
        foreach (var kv in Obj(d["macros"]))
        {
            macros[kv.Key] = ReadMacro(Obj(kv.Value));
        }
        return new SdkDescriptors(Json.AsString(d["target"]) ?? "", api, ops, macros);
    }

    // ------------------------------------------------------------ readers

    private static JsonObject Obj(JsonNode? v) => v as JsonObject ?? new JsonObject();

    private static JsonArray Arr(JsonNode? v) => v as JsonArray ?? new JsonArray();

    private static string? Str(JsonNode? v) => Json.AsString(v);

    private static bool Bool(JsonNode? v) => Json.TryBoolean(v, out var b) && b;

    private static long? Long(JsonNode? v) => Json.TryInteger(v, out var l) ? l : null;

    private static List<string> Strings(JsonNode? v) => Arr(v).Select(x => Str(x) ?? "").ToList();

    private static RemediationEntry Remedy(JsonObject o) => new()
    {
        Category = Wire.ParseCategory(Str(o["category"])),
        Text = Str(o["text"]),
        Retryable = Wire.ParseRetryable(Str(o["retryable"])),
        NextAction = Str(o["next_action"]),
    };

    private static Dictionary<string, RemediationEntry> Remedies(JsonNode? v) =>
        Obj(v).ToDictionary(kv => kv.Key, kv => Remedy(Obj(kv.Value)), StringComparer.Ordinal);

    private static RetryOptions Retry(JsonObject o) => new()
    {
        Max = Long(o["max"]) is long m ? (int)Math.Min(m, int.MaxValue) : null,
        Base = Long(o["base_ms"]) is long b ? TimeSpan.FromMilliseconds(b) : null,
        MaxDelay = Long(o["max_ms"]) is long x ? TimeSpan.FromMilliseconds(x) : null,
        Jitter = Str(o["jitter"]) switch
        {
            "none" => Runtime.Jitter.None,
            "equal" => Runtime.Jitter.Equal,
            "full" => Runtime.Jitter.Full,
            _ => null,
        },
        HonorRetryAfter = Json.TryBoolean(o["honor_retry_after"], out var h) ? h : null,
    };

    private static ApiDescriptor ReadApi(JsonObject o)
    {
        var auth = new List<AuthScheme>();
        foreach (var node in Arr(o["auth"]))
        {
            var s = Obj(node);
            var code = s["authorization_code"] as JsonObject;
            auth.Add(new AuthScheme
            {
                Kind = Str(s["kind"]) ?? "",
                Name = Str(s["name"]) ?? "",
                In = Str(s["in"]) ?? "header",
                Wire = Str(s["wire"]) ?? "",
                Prefix = Str(s["prefix"]),
                TokenUrl = Str(s["token_url"]),
                Scopes = Strings(s["scopes"]),
                AuthorizationCode = code is null ? null : new AuthorizationCodeFlow
                {
                    AuthorizationUrl = Str(code["authorization_url"]),
                    TokenUrl = Str(code["token_url"]),
                    RefreshUrl = Str(code["refresh_url"]),
                    Scopes = Strings(code["scopes"]),
                },
                Satisfies = Strings(s["satisfies"]),
                Parts = Arr(s["parts"]).Select(p =>
                {
                    var po = Obj(p);
                    return new CompositePart
                    {
                        Kind = Str(po["kind"]) ?? "",
                        Name = Str(po["name"]),
                        EqualsCookie = Str(po["equals_cookie"]),
                        FromConfig = Str(po["from_config"]),
                        MutationOnly = Bool(po["mutation_only"]),
                        Prefix = Str(po["prefix"]),
                    };
                }).ToList(),
            });
        }
        var nonJson = Arr(o["non_json"]).Select(n =>
        {
            var no = Obj(n);
            return new NonJsonError
            {
                Status = (int)(Long(no["status"]) ?? 0),
                Media = Str(no["media"]) ?? "none",
                Category = Wire.ParseCategory(Str(no["category"])) ?? Category.UnexpectedResponse,
                Retryable = Wire.ParseRetryable(Str(no["retryable"])) ?? Retryable.Never,
                Text = Str(no["text"]),
            };
        }).ToList();
        var retries = o["retries"] as JsonObject;
        return new ApiDescriptor
        {
            Name = Str(o["name"]) ?? "api",
            Version = Str(o["version"]) ?? "0",
            TungstenVersion = Str(o["tungsten_version"]) ?? "",
            Redirects = o["redirects"] is null ? "same_origin_read" : Str(o["redirects"]) ?? throw new ClientConfigException("redirects must be same_origin_read or none"),
            Servers = Strings(o["servers"]),
            Auth = auth,
            ErrorCodes = Remedies(o["error_codes"]),
            AmbiguousStatuses = Arr(o["ambiguous_statuses"]).Select(x => (int)(Long(x) ?? 0)).ToList(),
            NonJson = nonJson,
            Gates = Obj(o["gates"]).ToDictionary(kv => kv.Key, kv => Str(kv.Value) ?? "", StringComparer.Ordinal),
            Retries = retries is null ? null : new TierRetries(Retry(Obj(retries["read_only"])), Retry(Obj(retries["mutating"]))),
        };
    }

    private static OperationDescriptor ReadOperation(JsonObject o, IReadOnlyDictionary<string, Schema> defs)
    {
        var agent = Obj(o["agent"]);
        var sensitiveRequest = Strings(agent["sensitive_request_fields"]);
        var parameters = Arr(o["params"]).Select(p =>
        {
            var po = Obj(p);
            string key = Str(po["key"]) ?? Str(po["name"]) ?? "";
            return new ParamDescriptor
            {
                Key = key,
                Name = Str(po["name"]) ?? key,
                Wire = Str(po["wire"]) ?? "",
                In = Str(po["in"]) ?? "query",
                Required = Bool(po["required"]),
                Style = Str(po["style"]) ?? "form",
                Explode = Bool(po["explode"]),
                Role = Str(po["role"]) ?? "plain",
                Constant = Str(po["constant"]),
                Sensitive = sensitiveRequest.Contains(key),
            };
        }).ToList();
        BodyDescriptor? body = null;
        if (o["body"] is JsonObject b)
        {
            var shape = Obj(b["shape"]);
            bool merged = Str(shape["kind"]) == "merged";
            body = new BodyDescriptor
            {
                MediaType = Str(b["media_type"]) ?? "application/json",
                Encoding = Str(b["encoding"]) ?? "json",
                Required = Bool(b["required"]),
                Fields = merged
                    ? Arr(shape["fields"]).Select(f =>
                    {
                        var fo = Obj(f);
                        string key = Str(fo["key"]) ?? "";
                        return new MergedField(key, Str(fo["name"]) ?? key, Str(fo["wire"]) ?? key);
                    }).ToList()
                    : new List<MergedField>(),
                Arg = merged ? null : Str(shape["arg"]),
                ArgName = merged ? null : Str(shape["name"]),
            };
        }
        var responses = Arr(o["responses"]).Select(r =>
        {
            var ro = Obj(r);
            var status = ro["status"];
            string statusKind = "default";
            int code = 0;
            if (Json.TryInteger(status, out var exact))
            {
                statusKind = "exact";
                code = (int)exact;
            }
            else if (Str(status) is string text && text.Length == 3 && text.EndsWith("XX", StringComparison.Ordinal) && char.IsDigit(text[0]))
            {
                statusKind = "class";
                code = text[0] - '0';
            }
            return new ResponseDescriptor
            {
                StatusKind = statusKind,
                Code = code,
                Kind = Str(ro["kind"]) ?? "success",
                MediaType = Str(ro["media_type"]),
            };
        }).ToList();
        Pagination? pagination = null;
        if (o["pagination"] is JsonObject pg)
        {
            pagination = new Pagination
            {
                Style = Str(pg["style"]) ?? "cursor",
                RequestParam = Str(pg["request_param"]),
                ResponseField = Str(pg["response_field"]),
                ItemsField = Str(pg["items_field"]) ?? "",
                PageSizeParam = Str(pg["page_size_param"]),
                HasMoreField = Str(pg["has_more_field"]),
                CursorItemField = Str(pg["cursor_item_field"]),
                OffsetParam = Str(pg["offset_param"]),
                LimitParam = Str(pg["limit_param"]),
                PageParam = Str(pg["page_param"]),
                SizeParam = Str(pg["size_param"]),
            };
        }
        RpcDescriptor? rpc = null;
        if (o["rpc"] is JsonObject rp)
        {
            rpc = new RpcDescriptor
            {
                Field = Str(rp["field"]) ?? "",
                Value = Str(rp["value"]) ?? "",
                ParamsField = Str(rp["params_field"]) ?? "",
                Constants = rp["constants"] is JsonObject c ? Json.CopyObject(c) : null,
            };
        }
        Gate? gate = null;
        var statusObj = Obj(o["status"]);
        if (Str(statusObj["kind"]) == "gated")
        {
            gate = new Gate(Str(statusObj["env_var"]) ?? "", (int)(Long(statusObj["disabled_status"]) ?? 404));
        }
        var idem = Obj(agent["idempotency"]);
        var preview = Obj(agent["preview"]);
        var confirmation = agent["confirmation"] as JsonObject;
        var verify = agent["verify"] as JsonObject;
        StreamDescriptor? stream = null;
        if (o["stream"] is JsonObject st)
        {
            stream = new StreamDescriptor
            {
                Event = st.ContainsKey("event") ? Schema.FromJson(st["event"], defs) : null,
                Done = Str(st["done"]),
                Flag = Str(st["flag"]),
            };
        }
        return new OperationDescriptor
        {
            Id = Str(o["id"]) ?? "",
            Method = Str(o["method"]) ?? "GET",
            Path = Str(o["path"]) ?? "",
            Params = parameters,
            Body = body,
            Responses = responses,
            Security = Arr(o["security"]).Select(alt => (IReadOnlyList<string>)Strings(alt)).ToList(),
            Pagination = pagination,
            Rpc = rpc,
            ErrorCodeField = Str(o["error_code_field"]),
            Gate = gate,
            Agent = new AgentMeta
            {
                Safety = Wire.ParseSafety(Str(agent["safety"])) ?? Safety.Mutating,
                Idempotency = new IdempotencyMeta
                {
                    Policy = Wire.ParseIdempotency(Str(idem["policy"])),
                    Header = Str(idem["header"]),
                    Format = Str(idem["format"]),
                    PersistRequired = Bool(idem["persist_required"]),
                    Note = Str(idem["note"]),
                },
                Preview = new PreviewMode
                {
                    Mode = Str(preview["mode"]) ?? "local",
                    Header = Str(preview["header"]),
                    Value = Str(preview["value"]),
                    Operation = Str(preview["operation"]),
                },
                Confirmation = confirmation is null ? null : new ConfirmationMeta
                {
                    SummaryFields = Strings(confirmation["summary_fields"]),
                    Message = Str(confirmation["message"]),
                },
                Verify = verify is null ? null : new VerifyDescriptor
                {
                    Operation = Str(verify["operation"]) ?? "",
                    Args = Json.Copy(verify["args"]),
                    Expect = Json.Copy(verify["expect"]),
                    Terminal = Json.Copy(verify["terminal"]),
                    PollIntervalMs = Long(verify["poll_interval_ms"]),
                    PollBudgetMs = Long(verify["poll_budget_ms"]),
                },
                Remediation = Remedies(agent["remediation"]),
                RemediationNote = Str(agent["remediation_note"]),
                SensitiveResponseFields = Strings(agent["sensitive_response_fields"]),
                SensitiveRequestFields = sensitiveRequest,
                ShownOnce = Bool(agent["shown_once"]),
                ImportPolicy = ReadImportPolicy(agent["import_policy"]),
            },
            Request = o.ContainsKey("request") ? Schema.FromJson(o["request"], defs) : null,
            Response = o.ContainsKey("response") ? Schema.FromJson(o["response"], defs) : null,
            PageItem = o.ContainsKey("page_item") ? Schema.FromJson(o["page_item"], defs) : null,
            Stream = stream,
            Summary = Str(o["summary"]),
        };
    }

    private static ImportPolicy? ReadImportPolicy(JsonNode? raw)
    {
        if (raw is not JsonObject p)
        {
            return null;
        }
        // Only a literal true counts as reviewed; anything else fails closed.
        return new ImportPolicy
        {
            AllowedOrigins = Strings(p["allowed_origins"]),
            InferredSafety = Wire.ParseSafety(Str(p["inferred_safety"])) ?? Safety.Irreversible,
            Reviewed = Bool(p["reviewed"]),
            Source = Str(p["source"]) ?? "",
        };
    }

    private static MacroDescriptor ReadMacro(JsonObject o)
    {
        var input = Obj(o["input"]);
        var add = new JsonObject();
        var added = Obj(input["add"]);
        foreach (var k in Json.Keys(added).OrderBy(k => k, StringComparer.Ordinal))
        {
            Json.Put(add, k, added[k]);
        }
        return new MacroDescriptor
        {
            Name = Str(o["name"]) ?? "",
            Summary = Str(o["summary"]) ?? "",
            Safety = Wire.ParseSafety(Str(o["safety"])) ?? Safety.ReadOnly,
            Steps = Arr(o["steps"]).Select(s =>
            {
                var so = Obj(s);
                return new MacroStep
                {
                    Kind = Str(so["kind"]) ?? "call",
                    Operation = Str(so["operation"]) ?? "",
                    Args = Json.Copy(so["args"]),
                    As = Str(so["as"]),
                    Until = Json.Copy(so["until"]),
                    IntervalMs = Long(so["interval_ms"]),
                    BudgetMs = Json.Copy(so["budget_ms"]),
                    MaxPages = Long(so["max_pages"]),
                };
            }).ToList(),
            Output = Json.Copy(o["output"]),
            Input = new MacroInput
            {
                Extends = Str(input["extends"]),
                Add = add,
                Fields = Arr(input["fields"]).Select(f =>
                {
                    var fo = Obj(f);
                    string key = Str(fo["key"]) ?? "";
                    return new MacroInputField(key, Str(fo["name"]) ?? key, Bool(fo["required"]));
                }).ToList(),
            },
            SensitiveResponseFields = Strings(o["sensitive_response_fields"]),
            ShownOnce = Bool(o["shown_once"]),
            Cluster = Str(o["cluster"]),
        };
    }
}
