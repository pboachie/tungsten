// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"math"
	"strings"
)

const (
	defaultMacroBudgetMs = 30000.0
	macroPageLimit       = 100.0
)

func safetyRank(s Safety) int {
	switch s {
	case Mutating:
		return 1
	case Destructive:
		return 2
	case Irreversible:
		return 3
	}
	return 0
}

func usesKey(op *OperationDescriptor) bool {
	p := op.Agent.Idempotency.Policy
	return p == IdempotencyCallerOwned || p == IdempotencyAuto
}

type planStep struct {
	step MacroStep
	op   *OperationDescriptor
}

func macroInvalid(name, remediation string) *Error {
	return newDiag(name, ValidationFailed).param("macro").expected("a valid macro descriptor").remedy(remediation).err()
}

func (c *ClientCore) macroInput(m *MacroDescriptor, input any) (*Object, *Error) {
	converted, err := FromGo(input)
	if err != nil {
		converted = input
	}
	if nestsDeeperThan(converted, maxArgDepth) {
		return nil, newDiag(m.Name, ValidationFailed).param("input").
			expected(fmt.Sprintf("a JSON value nested at most %d levels", maxArgDepth)).
			remedy(fmt.Sprintf("Pass the input of %s without such deep nesting.", m.Name)).err()
	}
	var effective *Object
	switch t := converted.(type) {
	case *Object:
		effective = t.Clone()
	case nil:
		effective = NewObject()
	default:
		return nil, newDiag(m.Name, ValidationFailed).param("input").expected("an object").
			remedy(fmt.Sprintf("Pass the input of %s as one object.", m.Name)).err()
	}
	if m.Input.Add != nil {
		for _, key := range m.Input.Add.keys {
			if effective.Has(key) {
				continue
			}
			if schema, ok := m.Input.Add.vals[key].(*Object); ok {
				if def, ok := schema.Get("default"); ok {
					effective.Set(key, def)
				}
			}
		}
	}
	return effective, nil
}

func (c *ClientCore) macroPlan(m *MacroDescriptor) ([]planStep, Safety, *Error) {
	safety := m.Safety
	var steps []planStep
	for i, step := range m.Steps {
		op, ok := c.Operation(step.Operation)
		if !ok {
			return nil, safety, macroInvalid(m.Name, fmt.Sprintf("Step %d of %s names %s, which is not registered with the client.", i+1, m.Name, step.Operation))
		}
		if safetyRank(op.Agent.Safety) > safetyRank(safety) {
			safety = op.Agent.Safety
		}
		steps = append(steps, planStep{step, op})
	}
	return steps, safety, nil
}

func (c *ClientCore) stepArgs(m *MacroDescriptor, step MacroStep, op *OperationDescriptor, sc scope, pending map[string]bool) any {
	source := step.Args
	if source == nil {
		source = NewObject()
	}
	var evaluated any
	var ok bool
	if pending != nil {
		evaluated, ok = evaluateDry(source, sc, pending, 0)
	} else {
		evaluated, ok = evaluateExpr(source, sc, 0)
	}
	if !ok || evaluated == nil {
		evaluated = NewObject()
	}
	// The evaluated arguments may share members with the macro input (an
	// argument of "$input"): copy before changing them.
	evaluated = DeepCopy(evaluated)
	if o, isObj := evaluated.(*Object); isObj && op.ID == m.Input.Extends && m.Input.Add != nil {
		for _, key := range m.Input.Add.keys {
			o.Delete(key)
		}
	}
	return evaluated
}

func pageLimit(step MacroStep) int64 {
	var v any
	if step.MaxPages != nil {
		v = *step.MaxPages
	}
	return int64(math.Floor(bounded(v, step.MaxPages != nil, macroPageLimit, 1, 10000)))
}

func (c *ClientCore) previewMacro(ctx context.Context, m *MacroDescriptor, input any, opts CallOptions) (*Response[*PreviewResult], *Error) {
	name := m.Name
	steps, safety, err := c.macroPlan(m)
	if err != nil {
		return nil, err
	}
	effective, err := c.macroInput(m, input)
	if err != nil {
		return nil, err
	}
	if len(steps) == 0 {
		return nil, macroInvalid(name, fmt.Sprintf("%s has no steps; regenerate the SDK.", name))
	}
	rest := opts
	rest.Confirm = Confirm{}
	rest.Verify = false
	sc := NewObject()
	sc.Set("input", effective)
	pending := map[string]bool{}
	var previews []MacroStepPreview
	effects := []string{}
	if m.Summary != "" {
		effects = append(effects, m.Summary)
	}
	keyUsed := false
	for index, ps := range steps {
		step, op := ps.step, ps.op
		where := fmt.Sprintf("step %d of %s: %s", index+1, name, op.ID)
		args := c.stepArgs(m, step, op, sc, pending)
		stepOpts := rest
		if !usesKey(op) || keyUsed {
			stepOpts.IdempotencyKey = ""
		} else if opts.IdempotencyKey != "" {
			keyUsed = true
		}
		var request *RenderedRequest
		if obj, isObj := args.(*Object); isObj {
			unrenderable := op.Body != nil && (op.Body.Encoding == EncodeBytes || op.Body.Encoding == EncodeMultipart) && containsPlaceholder(obj)
			if !unrenderable {
				p, err := c.prepare(ctx, op, obj, stepOpts, purposeMacroPreview, "", nil)
				if err != nil {
					d := *err.Diagnostic
					d.Operation = name
					d.Remediation = fmt.Sprintf("%s (%s)", d.Remediation, where)
					return nil, &Error{Diagnostic: &d, Partial: err.Partial, HasPartial: err.HasPartial}
				}
				request = &RenderedRequest{
					Method:  p.method,
					URL:     unescapePlaceholders(p.displayURL, obj),
					Headers: p.headers.toMap(true),
					Body:    p.displayBody,
				}
			}
		} else if !containsPlaceholder(args) {
			return nil, newDiag(name, ValidationFailed).param("input").expected("an object").
				remedy(fmt.Sprintf("Step %d of %s does not evaluate to an argument object.", index+1, name)).err()
		}
		own := []string{}
		if conf := op.Agent.Confirmation; conf != nil && conf.Message != "" {
			obj, _ := args.(*Object)
			if obj == nil {
				obj = NewObject()
			}
			own = append(own, interpolate(conf.Message, obj, op))
		}
		switch step.Kind {
		case "poll":
			until := ""
			if step.Until != nil {
				until = describePredicate(step.Until)
			}
			bv, bok := evaluateExpr(step.BudgetMs, sc, 0)
			budget := bounded(bv, bok, defaultMacroBudgetMs, 0, maxSafeInteger)
			var iv any
			if step.IntervalMs != nil {
				iv = *step.IntervalMs
			}
			interval := bounded(iv, step.IntervalMs != nil, defaultPollIntervalMs, 0, maxSafeInteger)
			untilText := ""
			if until != "" {
				untilText = " until " + until
			}
			own = append(own, fmt.Sprintf("Repeats %s every %s ms%s, for at most %s ms.", op.ID, JSNumber(interval), untilText, JSNumber(budget)))
		case "paginate":
			own = append(own, fmt.Sprintf("Reads up to %d pages of %s.", pageLimit(step), op.ID))
		}
		if op.Agent.RemediationNote != "" {
			own = append(own, op.Agent.RemediationNote)
		}
		effects = append(effects, fmt.Sprintf("Step %d: %s %s (%s).", index+1, step.Kind, op.ID, op.Agent.Safety))
		effects = append(effects, own...)
		previews = append(previews, MacroStepPreview{
			Step: index + 1, Kind: step.Kind, Operation: op.ID, As: step.As, Safety: op.Agent.Safety,
			Request: request, Effects: own,
		})
		if step.As != "" {
			pending[step.As] = true
		}
	}
	if m.ShownOnce {
		effects = append(effects, "The result contains values the API shows only once; store them immediately.")
	}
	if previews[0].Request == nil {
		return nil, newDiag(name, ValidationFailed).param("input").expected("an object").
			remedy(fmt.Sprintf("Step 1 of %s cannot be rendered.", name)).err()
	}
	result := &PreviewResult{
		Operation: name,
		Safety:    safety,
		Request:   *previews[0].Request,
		Effects:   effects,
		Steps:     previews,
	}
	if safety != ReadOnly {
		result.ConfirmationToken = IssueToken(c.confirmationKey, "macro:"+name, effective, c.now())
		result.ExpiresInMs = ConfirmationTTLMs
	}
	return &Response[*PreviewResult]{Value: result, HasValue: true, Meta: localMeta()}, nil
}

// rerunProtected: running the step again (by rerunning its macro with the
// same input and options) answers the first result instead of applying the
// effect twice.
func rerunProtected(op *OperationDescriptor, opts CallOptions) bool {
	switch op.Agent.Idempotency.Policy {
	case IdempotencyContentIdentity, IdempotencyContentHash, IdempotencyAuto:
		return true
	}
	if opts.IdempotencyKey == "" {
		return false
	}
	if op.Agent.Idempotency.Policy == IdempotencyCallerOwned {
		return true
	}
	for _, p := range op.Params {
		if p.Role == RoleIdempotencyKey {
			return true
		}
	}
	return false
}

// macroBind is the replay protection a macro's token is bound to: the
// run's key when a step sends it and every mutating step is protected
// against a rerun; otherwise none.
func macroBind(plan []planStep, opts CallOptions) (string, bool) {
	if opts.IdempotencyKey == "" {
		return "", false
	}
	sentKey := false
	for _, ps := range plan {
		stepOpts := opts
		if !usesKey(ps.op) || sentKey {
			stepOpts.IdempotencyKey = ""
		} else {
			sentKey = true
		}
		if isMutation(ps.op) && !rerunProtected(ps.op, stepOpts) {
			return "", false
		}
	}
	if !sentKey {
		return "", false
	}
	return opts.IdempotencyKey, true
}

func (c *ClientCore) runMacro(ctx context.Context, m *MacroDescriptor, input any, opts CallOptions) (*Outcome, *Error) {
	name := m.Name
	steps, safety, err := c.macroPlan(m)
	if err != nil {
		return nil, err
	}
	effective, err := c.macroInput(m, input)
	if err != nil {
		return nil, err
	}
	if err := c.confirmed(confirmTarget{id: name, subject: "macro:" + name, previewCall: "the macro's preview(...)"}, safety, effective, opts); err != nil {
		return nil, err
	}
	bind, hasBind := macroBind(steps, opts)
	claim := &macroClaim{name: name, token: opts.Confirm.token, bind: bind, hasBind: hasBind}
	sc := NewObject()
	sc.Set("input", effective)
	var completed []string
	produced := NewObject()
	var unprotected []string
	keyUsed := false
	meta := ResponseMeta{Headers: map[string]string{}}
	for index, ps := range steps {
		step, op := ps.step, ps.op
		args := c.stepArgs(m, step, op, sc, nil)
		if _, isObj := args.(*Object); !isObj {
			return nil, newDiag(name, ValidationFailed).param("input").expected("an object").
				remedy(fmt.Sprintf("Step %d of %s does not evaluate to an argument object.", index+1, name)).err()
		}
		stepOpts := opts
		stepOpts.Confirm = Confirm{}
		stepOpts.Verify = false
		if !usesKey(op) || keyUsed {
			stepOpts.IdempotencyKey = ""
		} else if opts.IdempotencyKey != "" {
			keyUsed = true
		}
		var value any
		hasValue := false
		var failure *Error
		switch step.Kind {
		case "poll":
			bv, bok := evaluateExpr(step.BudgetMs, sc, 0)
			budget := bounded(bv, bok, defaultMacroBudgetMs, 0, maxSafeInteger)
			var iv any
			if step.IntervalMs != nil {
				iv = *step.IntervalMs
			}
			interval := bounded(iv, step.IntervalMs != nil, defaultPollIntervalMs, 0, maxSafeInteger)
			until := any(NewObject())
			if u, ok := step.Until.(*Object); ok {
				until = u
			}
			polled, err := c.pollWith(ctx, op, args, pollSpec{until: until, interval: durationMs(interval), budget: durationMs(budget)}, stepOpts, claim)
			if err != nil {
				failure = err
			} else {
				if polled.Value.TimedOut {
					value, hasValue = nil, true
				} else {
					value, hasValue = polled.Value.Body, polled.Value.HasBody
				}
				meta = polled.Meta
			}
		case "paginate":
			items := []any{}
			limit := pageLimit(step)
			var pages int64
			st := newPageState(args)
			for {
				page, err, more := c.pageStep(ctx, op, st, stepOpts, claim)
				if !more {
					break
				}
				if err != nil {
					failure = err
					break
				}
				items = append(items, page.Value.Items...)
				meta = page.Meta
				pages++
				if pages >= limit {
					break
				}
			}
			value, hasValue = items, true
		default:
			response, err := c.callWith(ctx, op, args, stepOpts, "", claim)
			if err != nil {
				failure = err
			} else {
				value, hasValue = response.Value, response.HasValue
				meta = response.Meta
			}
		}
		if failure != nil {
			d := *failure.Diagnostic
			if failure.HasPartial && step.As != "" {
				produced.Set(step.As, failure.Partial)
			}
			done := "no step completed before it"
			if len(completed) > 0 {
				done = "completed before it: " + strings.Join(completed, ", ")
			}
			remediation := fmt.Sprintf("%s Macro %s stopped at step %d of %d (%s); %s.", d.Remediation, name, index+1, len(steps), op.ID, done)
			retryable := d.Retryable
			next := d.NextAction
			if kept := produced.Keys(); len(kept) > 0 {
				once := ""
				if m.ShownOnce {
					once = ", including values the API shows only once: store them now"
				}
				remediation += fmt.Sprintf(" The result's partial holds the completed steps' results (%s)%s.", strings.Join(kept, ", "), once)
			}
			if len(unprotected) > 0 {
				remediation += fmt.Sprintf(" Do not run %s again: %s already took effect and has no replay protection.", name, strings.Join(unprotected, ", "))
				if retryable == RetryAfterDelay || retryable == RetrySameKeyOnly {
					retryable = RetryAfterRemediation
				}
				finish := fmt.Sprintf("Finish %s without rerunning it: call %s yourself with the values from the result's partial.", name, op.ID)
				if next == nil {
					next = &finish
				} else {
					next = strPtr(*next + " " + finish)
				}
			}
			d.Operation = name
			d.Remediation = remediation
			d.Retryable = retryable
			d.NextAction = next
			if produced.Len() > 0 {
				return nil, &Error{Diagnostic: &d, Partial: produced, HasPartial: true}
			}
			return nil, &Error{Diagnostic: &d}
		}
		completed = append(completed, op.ID)
		if step.As != "" {
			if hasValue {
				sc.Set(step.As, value)
				produced.Set(step.As, value)
			} else {
				sc.Delete(step.As)
				produced.Set(step.As, nil)
			}
		}
		if isMutation(op) && !rerunProtected(op, stepOpts) {
			unprotected = append(unprotected, op.ID)
		}
	}
	output, ok := evaluateExpr(m.Output, sc, 0)
	return &Outcome{Value: output, HasValue: ok, Raw: output, Meta: meta}, nil
}
