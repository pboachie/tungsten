// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Tungsten.Runtime;

internal sealed class PageState
{
    public PageState(JsonNode? args)
    {
        Args = args is JsonObject o ? (JsonObject)Json.Normalize(o)! : args is null ? new JsonObject() : null;
        Raw = args;
    }

    /// <summary>The arguments, updated page by page; null when they are not an object (the call refuses them).</summary>
    public JsonObject? Args { get; }

    public JsonNode? Raw { get; }
    public string? OverrideUrl { get; set; }
    public JsonNode? PreviousCursor { get; set; }
    public bool HasPrevious { get; set; }
    public bool Done { get; set; }
    public TungstenError? PendingError { get; set; }
}

public sealed partial class ClientCore
{
    private static JsonNode AsNumber(double n) =>
        n == Math.Truncate(n) && Math.Abs(n) < Json.MaxSafeInteger ? JsonValue.Create((long)n) : JsonValue.Create(n);

    /// <summary>One page; null after the last page.</summary>
    internal async Task<Result<Page<JsonNode?>>?> PageStepAsync(OperationDescriptor op, PageState st, CallOptions opts, MacroClaim? claim, CancellationToken ct)
    {
        if (st.PendingError is { } pending)
        {
            st.PendingError = null;
            st.Done = true;
            return new Result<Page<JsonNode?>>.Err(pending);
        }
        if (st.Done)
        {
            return null;
        }
        Result<JsonNode?>.Ok response;
        try
        {
            response = await CallWithAsync(op, st.Args ?? st.Raw, opts, st.OverrideUrl, claim, ct).ConfigureAwait(false);
        }
        catch (Failure)
        {
            st.Done = true;
            throw;
        }
        var body = response.Value;
        var pg = op.Pagination;
        var itemsField = pg?.ItemsField ?? "";
        JsonNode? found;
        bool foundOk = true;
        if (itemsField.Length == 0)
        {
            found = body;
        }
        else
        {
            foundOk = Util.GetPathStr(body, response.HasValue, itemsField, out found);
        }
        var items = new List<JsonNode?>();
        if (foundOk && found is JsonArray list)
        {
            items.AddRange(list.Select(Json.Copy));
        }
        JsonNode? next = null;
        var args = st.Args;
        if (pg is not null && args is not null)
        {
            switch (pg.Style)
            {
                case "cursor":
                    {
                        static bool Usable(JsonNode? v) => !Json.IsNull(v) && !(Json.TryString(v, out var s) && s.Length == 0);
                        JsonNode? cursor = null;
                        bool has = false;
                        if (!string.IsNullOrEmpty(pg.ResponseField) && Util.GetPathStr(body, response.HasValue, pg.ResponseField, out var c) && Usable(c))
                        {
                            cursor = c;
                            has = true;
                        }
                        if (!has && !string.IsNullOrEmpty(pg.CursorItemField) && items.Count > 0 &&
                            Util.GetPathStr(items[^1], true, pg.CursorItemField, out var ic) && Usable(ic))
                        {
                            cursor = ic;
                            has = true;
                        }
                        bool finished = !string.IsNullOrEmpty(pg.HasMoreField) && Util.GetPathStr(body, response.HasValue, pg.HasMoreField, out var more) &&
                            Json.TryBoolean(more, out var b) && !b;
                        if (has && !finished && !(st.HasPrevious && Json.Canonical(st.PreviousCursor) == Json.Canonical(cursor)))
                        {
                            next = Json.Copy(cursor);
                            st.PreviousCursor = Json.Copy(cursor);
                            st.HasPrevious = true;
                            Json.Put(args, op.ArgName(pg.RequestParam ?? ""), cursor);
                        }
                        break;
                    }
                case "offset":
                    {
                        var offsetKey = op.ArgName(pg.OffsetParam ?? "");
                        bool hasLimit = args.TryGetPropertyValue(op.ArgName(pg.LimitParam ?? ""), out var limitValue);
                        bool limitIsNum = Json.TryNumber(limitValue, out var limit);
                        bool ook = args.TryGetPropertyValue(offsetKey, out var ov);
                        double offset = Util.Bounded(ov, ook, 0, 0, Json.MaxSafeInteger);
                        bool shortPage = hasLimit && limitIsNum && items.Count < limit;
                        if (items.Count > 0 && !shortPage)
                        {
                            var v = AsNumber(offset + items.Count);
                            Json.Put(args, offsetKey, v);
                            next = Json.Copy(v);
                        }
                        break;
                    }
                case "page":
                    {
                        var pageKey = op.ArgName(pg.PageParam ?? "");
                        bool hasSize = args.TryGetPropertyValue(op.ArgName(pg.SizeParam ?? ""), out var sizeValue);
                        bool sizeIsNum = Json.TryNumber(sizeValue, out var size);
                        bool pok = args.TryGetPropertyValue(pageKey, out var pv);
                        double page = Util.Bounded(pv, pok, 1, 0, Json.MaxSafeInteger);
                        bool shortPage = hasSize && sizeIsNum && items.Count < size;
                        if (items.Count > 0 && !shortPage)
                        {
                            var v = AsNumber(page + 1);
                            Json.Put(args, pageKey, v);
                            next = Json.Copy(v);
                        }
                        break;
                    }
                case "link_header":
                    {
                        response.Meta.Headers.TryGetValue("link", out var linkHeader);
                        if (Transport.NextLink(linkHeader) is string link)
                        {
                            var baseText = st.OverrideUrl ?? baseUrl;
                            if (Uri.TryCreate(baseText, UriKind.Absolute, out var origin) && Uri.TryCreate(origin, link, out var resolved) && Urls.SameOrigin(origin, resolved))
                            {
                                st.OverrideUrl = resolved.AbsoluteUri;
                                next = JsonValue.Create(resolved.AbsoluteUri);
                                break;
                            }
                            st.PendingError = new TungstenError(new Diag(op.Id, Category.UnexpectedResponse).Status(response.Meta.Status)
                                .RequestId(response.Meta.RequestId)
                                .Remedy("The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. Stop paginating here.")
                                .Attempts(response.Meta.Attempts).Build());
                        }
                        break;
                    }
            }
        }
        if (CheckItems(op, itemsField, items, response) is Failure f)
        {
            st.Done = true;
            throw f;
        }
        if (next is null)
        {
            st.Done = true;
        }
        return new Result<Page<JsonNode?>>.Ok(new Page<JsonNode?> { Items = items, RawItems = items, Body = body, Next = next })
        {
            HasValue = true,
            Raw = body,
            Meta = response.Meta,
        };
    }

    /// <summary>Validates page items with the operation's page item schema.</summary>
    private Failure? CheckItems(OperationDescriptor op, string itemsField, List<JsonNode?> items, Result<JsonNode?>.Ok response)
    {
        var mode = validateResponses;
        if (op.PageItem is not { } schema || mode == ValidateResponses.Off)
        {
            return null;
        }
        for (int index = 0; index < items.Count; index++)
        {
            var issues = Judge(schema, items[index]);
            if (issues.Count == 0)
            {
                continue;
            }
            var issue = issues[0];
            var path = "response" + (itemsField.Length > 0 ? "." + itemsField : "") + "[" + index.ToString(CultureInfo.InvariantCulture) + "]";
            foreach (var seg in issue.Path)
            {
                path += seg is int i ? "[" + i.ToString(CultureInfo.InvariantCulture) + "]" : "." + seg;
            }
            var d = new Diag(op.Id, Category.UnexpectedResponse).Status(response.Meta.Status).RequestId(response.Meta.RequestId)
                .Param(path).Expected(issue.Message)
                .Remedy($"A page item does not match the API description at {path} ({issue.Message}).")
                .Attempts(response.Meta.Attempts).Build();
            if (mode == ValidateResponses.Strict)
            {
                return new Failure(d);
            }
            Emit(d);
            return null;
        }
        return null;
    }
}
