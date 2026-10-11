// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"strings"
	"time"
)

func localMeta() ResponseMeta {
	return ResponseMeta{Headers: map[string]string{}}
}

func (c *ClientCore) preview(ctx context.Context, op *OperationDescriptor, args any, opts CallOptions) (*Response[*PreviewResult], *Error) {
	p, err := c.prepare(ctx, op, args, opts, purposePreview, "", nil)
	if err != nil {
		return nil, err
	}
	var effects []string
	if conf := op.Agent.Confirmation; conf != nil && conf.Message != "" {
		effects = append(effects, interpolate(conf.Message, p.args, op))
	}
	if op.Agent.RemediationNote != "" {
		effects = append(effects, op.Agent.RemediationNote)
	}
	if conf := op.Agent.Confirmation; conf != nil && len(conf.SummaryFields) > 0 {
		fields := make([]string, len(conf.SummaryFields))
		for i, f := range conf.SummaryFields {
			fields[i] = fmt.Sprintf("%s={%s}", f, f)
		}
		effects = append(effects, interpolate("Arguments: "+strings.Join(fields, ", ")+".", p.args, op))
	}
	if op.Summary != "" {
		effects = append(effects, op.Summary)
	}
	if effects == nil {
		effects = []string{}
	}
	result := &PreviewResult{
		Operation: op.ID,
		Safety:    op.Agent.Safety,
		Request:   RenderedRequest{Method: p.method, URL: p.displayURL, Headers: p.headers.toMap(true), Body: p.displayBody},
		Effects:   effects,
	}
	if op.Agent.Safety != ReadOnly {
		result.ConfirmationToken = IssueToken(c.confirmationKey, op.ID, p.args, c.now())
		result.ExpiresInMs = ConfirmationTTLMs
	}
	switch op.Agent.Preview.Mode {
	case "header":
		dry, err := c.prepare(ctx, op, args, opts, purposeServerPreview, "", nil)
		if err != nil {
			return nil, err
		}
		answer, err := c.send(ctx, dry, opts)
		if err != nil {
			return nil, err
		}
		if answer.HasValue {
			result.ServerPreview = redactPaths(answer.Value, op.Agent.SensitiveResponseFields)
			result.HasServerPreview = true
		}
		return &Response[*PreviewResult]{Value: result, HasValue: true, Meta: answer.Meta}, nil
	case "endpoint":
		target, ok := c.Operation(op.Agent.Preview.Operation)
		if !ok {
			name := op.Agent.Preview.Operation
			return nil, newDiag(op.ID, ValidationFailed).param("operation").
				expected(fmt.Sprintf("the preview operation %s registered with the client", name)).
				remedy(fmt.Sprintf("Register %s (ClientCore.Register or ClientOptions.Operations) so preview() can call it; nothing was sent.", name)).err()
		}
		rest := opts
		rest.Confirm = Confirm{}
		rest.Verify = false
		answer, err := c.callWith(ctx, target, args, rest, "", nil)
		if err != nil {
			return nil, err
		}
		result.ServerPreview, result.HasServerPreview = answer.Value, answer.HasValue
		return &Response[*PreviewResult]{Value: result, HasValue: true, Meta: answer.Meta}, nil
	}
	return &Response[*PreviewResult]{Value: result, HasValue: true, Meta: localMeta()}, nil
}

const (
	defaultPollIntervalMs = 1000.0
	defaultVerifyBudgetMs = 30000.0
)

// unresolved stands in for a verification reference that did not resolve;
// it equals nothing a response can hold.
func unresolved() *Object {
	o := NewObject()
	o.Set("$tungsten_unresolved", true)
	return o
}

func resolveRefs(node any, sc scope, depth int) any {
	if depth > 64 {
		return unresolved()
	}
	switch t := node.(type) {
	case string:
		if strings.HasPrefix(t, "$") {
			if v, ok := resolveRef(t, sc); ok && v != nil {
				return v
			}
			return unresolved()
		}
		return t
	case []any:
		out := make([]any, len(t))
		for i, item := range t {
			out[i] = resolveRefs(item, sc, depth+1)
		}
		return out
	case *Object:
		out := NewObject()
		for _, k := range t.keys {
			out.Set(k, resolveRefs(t.vals[k], sc, depth+1))
		}
		return out
	}
	return node
}

type pollSpec struct {
	until    any
	interval time.Duration
	budget   time.Duration
}

func (c *ClientCore) pollWith(ctx context.Context, op *OperationDescriptor, args any, spec pollSpec, opts CallOptions, claim *macroClaim) (*Response[Polled], *Error) {
	started := time.Now()
	once := opts
	once.Verify = false
	for {
		response, err := c.callWith(ctx, op, args, once, "", claim)
		if err != nil {
			return nil, err
		}
		done := EvaluatePredicate(spec.until, response.Value, response.HasValue)
		outOfTime := !done && time.Since(started)+spec.interval > spec.budget
		if done || outOfTime {
			return &Response[Polled]{
				Value:    Polled{Body: response.Value, HasBody: response.HasValue, TimedOut: outOfTime},
				HasValue: true,
				Meta:     response.Meta,
			}, nil
		}
		timer := time.NewTimer(spec.interval)
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, newDiag(op.ID, TransportFailed).retry(RetryNever).
				remedy("The poll was cancelled by the caller's context before its predicate held.").err()
		case <-timer.C:
		}
	}
}

func (c *ClientCore) verify(ctx context.Context, op *OperationDescriptor, args *Object, value any, hasValue bool, opts CallOptions) Verification {
	unchecked := Verification{}
	hook := op.Agent.Verify
	if hook == nil {
		return unchecked
	}
	target, ok := c.Operation(hook.Operation)
	if !ok {
		return unchecked
	}
	// The hook is written against the API: argument keys and $args
	// references use wire names.
	sc := NewObject()
	if hasValue {
		sc.Set("response", value)
	}
	sc.Set("args", withWireNames(op, args))
	hookArgs := hook.Args
	if hookArgs == nil {
		hookArgs = NewObject()
	}
	evaluated, _ := evaluateExpr(hookArgs, sc, 0)
	evaluatedObj, ok := evaluated.(*Object)
	if !ok {
		return unchecked
	}
	mapped := NewObject()
	for _, k := range evaluatedObj.keys {
		mapped.Set(argName(target, k), evaluatedObj.vals[k])
	}
	resolve := func(predicate any) any {
		if o, ok := predicate.(*Object); ok && o.Len() > 0 {
			return resolveRefs(predicate, sc, 0)
		}
		return nil
	}
	terminal := resolve(hook.Terminal)
	expect := resolve(hook.Expect)
	if expect == nil {
		expect = NewObject()
	}
	interval := durationMs(defaultPollIntervalMs)
	if hook.PollInterval != nil {
		interval = *hook.PollInterval
	}
	var budget time.Duration
	if hook.PollBudget != nil {
		budget = *hook.PollBudget
	} else if terminal != nil {
		budget = durationMs(defaultVerifyBudgetMs)
	}
	rest := opts
	rest.Confirm = Confirm{}
	rest.IdempotencyKey = ""
	rest.Verify = false
	until := expect
	if terminal != nil {
		until = terminal
	}
	polled, err := c.pollWith(ctx, target, mapped, pollSpec{until: until, interval: interval, budget: budget}, rest, nil)
	if err != nil {
		unchecked.Error = err.Diagnostic
		return unchecked
	}
	body := polled.Value
	out := Verification{
		Checked:  true,
		Passed:   EvaluatePredicate(expect, body.Body, body.HasBody),
		TimedOut: budget != 0 && body.TimedOut,
	}
	if body.HasBody {
		out.Observed = redactPaths(body.Body, target.Agent.SensitiveResponseFields)
		out.HasObserved = true
	}
	return out
}
