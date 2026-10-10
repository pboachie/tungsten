// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"math"
	"net/url"
)

type pageState struct {
	args           any
	overrideURL    string
	previousCursor any
	hasPrevious    bool
	done           bool
	pendingError   *Error
}

func newPageState(args any) *pageState {
	if converted, err := FromGo(args); err == nil {
		args = converted
	}
	if args == nil {
		args = NewObject()
	}
	if o, ok := args.(*Object); ok {
		args = o.Clone()
	}
	return &pageState{args: args}
}

// Pages iterates the pages of an operation, like bufio.Scanner:
//
//	for pages.Next(ctx) {
//		page := pages.Page()
//	}
//	if err := pages.Err(); err != nil { ... }
//
// The first error ends the iteration.
type Pages struct {
	core  *ClientCore
	op    *OperationDescriptor
	opts  CallOptions
	state *pageState
	page  *Response[Page[any]]
	err   *Error
}

// Next fetches the next page; false after the last page or an error.
func (p *Pages) Next(ctx context.Context) bool {
	p.page = nil
	if p.err != nil {
		return false
	}
	page, err, more := p.core.pageStep(ctx, p.op, p.state, p.opts, nil)
	if err != nil {
		p.err = err
		return false
	}
	p.page = page
	return more
}

// Page is the page Next fetched.
func (p *Pages) Page() *Response[Page[any]] { return p.page }

// Err is the error that ended the iteration, or nil.
func (p *Pages) Err() error {
	if p.err == nil {
		return nil
	}
	return p.err
}

// Operation is the paginated operation's descriptor.
func (p *Pages) Operation() *OperationDescriptor { return p.op }

// All collects every page; an error ends the collection.
func (p *Pages) All(ctx context.Context) ([]*Response[Page[any]], error) {
	var out []*Response[Page[any]]
	for p.Next(ctx) {
		out = append(out, p.Page())
	}
	return out, p.Err()
}

func asNumber(n float64) any {
	if n == math.Trunc(n) && math.Abs(n) < maxSafeInteger {
		return int64(n)
	}
	return n
}

func setArg(args any, key string, value any) {
	if o, ok := args.(*Object); ok {
		o.Set(key, value)
	}
}

func argAt(args any, key string) (any, bool) {
	o, ok := args.(*Object)
	if !ok {
		return nil, false
	}
	return o.Get(key)
}

func (c *ClientCore) pageStep(ctx context.Context, op *OperationDescriptor, st *pageState, opts CallOptions, claim *macroClaim) (*Response[Page[any]], *Error, bool) {
	if st.pendingError != nil {
		err := st.pendingError
		st.pendingError = nil
		st.done = true
		return nil, err, true
	}
	if st.done {
		return nil, nil, false
	}
	response, err := c.callWith(ctx, op, st.args, opts, st.overrideURL, claim)
	if err != nil {
		st.done = true
		return nil, err, true
	}
	body := response.Value
	itemsField := ""
	if op.Pagination != nil {
		itemsField = op.Pagination.ItemsField
	}
	var found any
	foundOK := true
	if itemsField == "" {
		found = body
	} else {
		found, foundOK = getPathStr(body, response.HasValue, itemsField)
	}
	items := []any{}
	if list, ok := found.([]any); ok && foundOK {
		items = append(items, list...)
	}
	var next any
	if pg := op.Pagination; pg != nil {
		switch pg.Style {
		case "cursor":
			usable := func(v any) bool {
				s, isText := v.(string)
				return v != nil && !(isText && s == "")
			}
			var cursor any
			has := false
			if pg.ResponseField != "" {
				if v, ok := getPathStr(body, response.HasValue, pg.ResponseField); ok && usable(v) {
					cursor, has = v, true
				}
			}
			if !has && pg.CursorItemField != "" && len(items) > 0 {
				if v, ok := getPathStr(items[len(items)-1], true, pg.CursorItemField); ok && usable(v) {
					cursor, has = v, true
				}
			}
			finished := false
			if pg.HasMoreField != "" {
				if v, ok := getPathStr(body, response.HasValue, pg.HasMoreField); ok {
					if b, isBool := v.(bool); isBool && !b {
						finished = true
					}
				}
			}
			if has && !finished && !(st.hasPrevious && CanonicalJSON(st.previousCursor) == CanonicalJSON(cursor)) {
				next = cursor
				st.previousCursor, st.hasPrevious = cursor, true
				setArg(st.args, argName(op, pg.RequestParam), cursor)
			}
		case "offset":
			offsetKey := argName(op, pg.OffsetParam)
			limitValue, hasLimit := argAt(st.args, argName(op, pg.LimitParam))
			limit, limitIsNum := asFloat(limitValue)
			ov, ook := argAt(st.args, offsetKey)
			offset := bounded(ov, ook, 0, 0, maxSafeInteger)
			short := hasLimit && limitIsNum && float64(len(items)) < limit
			if len(items) > 0 && !short {
				v := asNumber(offset + float64(len(items)))
				setArg(st.args, offsetKey, v)
				next = v
			}
		case "page":
			pageKey := argName(op, pg.PageParam)
			sizeValue, hasSize := argAt(st.args, argName(op, pg.SizeParam))
			size, sizeIsNum := asFloat(sizeValue)
			pv, pok := argAt(st.args, pageKey)
			page := bounded(pv, pok, 1, 0, maxSafeInteger)
			short := hasSize && sizeIsNum && float64(len(items)) < size
			if len(items) > 0 && !short {
				v := asNumber(page + 1)
				setArg(st.args, pageKey, v)
				next = v
			}
		case "link_header":
			if link, ok := nextLink(response.Meta.Headers["link"]); ok {
				base := st.overrideURL
				if base == "" {
					base = c.baseURL
				}
				origin, err1 := url.Parse(base)
				ref, err2 := url.Parse(link)
				if err1 == nil && err2 == nil && origin.Scheme != "" {
					resolved := origin.ResolveReference(ref)
					if sameOrigin(origin, resolved) {
						target := resolved.String()
						st.overrideURL = target
						next = target
						break
					}
				}
				st.pendingError = newDiag(op.ID, UnexpectedResponse).status(response.Meta.Status).
					requestID(response.Meta.RequestID).
					remedy("The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. Stop paginating here.").
					attempts(response.Meta.Attempts).err()
			}
		}
	}
	if err := c.checkItems(op, itemsField, items, response); err != nil {
		st.done = true
		return nil, err, true
	}
	if next == nil {
		st.done = true
	}
	return &Response[Page[any]]{
		Value:        Page[any]{Items: items, Body: body, Next: next},
		HasValue:     true,
		Raw:          body,
		Meta:         response.Meta,
		Verification: nil,
	}, nil, true
}

// checkItems validates page items with the operation's PageItem validator.
func (c *ClientCore) checkItems(op *OperationDescriptor, itemsField string, items []any, response *Outcome) *Error {
	mode := c.validateResponses
	if op.PageItem == nil || mode == ValidateOff {
		return nil
	}
	for index, item := range items {
		issues := judge(op.PageItem, item)
		if len(issues) == 0 {
			continue
		}
		issue := issues[0]
		path := "response"
		if itemsField != "" {
			path += "." + itemsField
		}
		path += fmt.Sprintf("[%d]", index)
		for _, seg := range issue.Path {
			switch t := seg.(type) {
			case int:
				path += fmt.Sprintf("[%d]", t)
			case string:
				path += "." + t
			}
		}
		d := newDiag(op.ID, UnexpectedResponse).status(response.Meta.Status).requestID(response.Meta.RequestID).
			param(path).expected(issue.Message).
			remedy(fmt.Sprintf("A page item does not match the API description at %s (%s).", path, issue.Message)).
			attempts(response.Meta.Attempts).build()
		if mode == ValidateStrict {
			return newError(d)
		}
		c.emit(d)
		return nil
	}
	return nil
}
