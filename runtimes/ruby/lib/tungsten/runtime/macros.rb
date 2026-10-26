# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    class ClientCore
      DEFAULT_MACRO_BUDGET_MS = 30_000
      MACRO_PAGE_LIMIT = 100
      SAFETY_RANK = { "read_only" => 0, "mutating" => 1, "destructive" => 2, "irreversible" => 3 }.freeze

      # A macro run's one-time spend of its confirmation token: the first
      # step about to send claims it.
      class MacroClaim
        def initialize(name, token, bind)
          @name = name
          @token = token
          @bind = bind
          @lock = Mutex.new
          @claimed = false
        end

        def claim(core)
          return nil if @token.nil?

          @lock.synchronize do
            return nil if @claimed

            @claimed = true
            core.send(:claim_token, @name, @token, @bind, "the macro's preview(...)")
          end
        end
      end

      # Run a macro by name (or descriptor) with its input keyed by the SDK's
      # names: Ok with its output, or Err with the failing step's envelope
      # and the completed steps' results as `partial`. A destructive or
      # irreversible macro needs `confirm`.
      def run_macro(macro, input = {}, opts = nil)
        with_macro(macro, opts) { |m, o| run_macro_with(m, input, o) }
      end

      # Preview a macro without sending anything.
      def preview_macro(macro, input = {}, opts = nil)
        with_macro(macro, opts) { |m, o| preview_macro_with(m, input, o) }
      end

      private

      def with_macro(macro, opts)
        m = macro.is_a?(MacroDescriptor) ? macro : @descriptors.macro(macro.to_s)
        unless m
          return Runtime.diag(macro.to_s, "VALIDATION_FAILED").param("macro").expected("a macro name of this SDK")
                        .remedy("This SDK has no macro #{macro}; check the name. Nothing was sent.").err
        end
        begin
          o = CallOptions.from(opts)
        rescue ArgumentError => e
          return Runtime.diag(m.name, "VALIDATION_FAILED").param("opts").expected("call options")
                        .remedy("Pass the call options as a Hash of idempotency_key, confirm, timeout_ms, verify and headers (#{e.message}).").err
        end
        yield m, with_deadline(o)
      end

      def uses_key?(op) = %w[caller_owned auto].include?(op.agent["idempotency"]["policy"])

      def macro_invalid(name, remediation)
        Runtime.diag(name, "VALIDATION_FAILED").param("macro").expected("a valid macro descriptor").remedy(remediation).err
      end

      # The input keyed by the arguments layout keys, with the added
      # fields' defaults, or an Err.
      def macro_input(m, input)
        converted =
          begin
            Json.from_ruby(input.nil? ? {} : input)
          rescue ArgumentError
            UNSET
          end
        if Util.nests_deeper?(converted, Util::MAX_ARG_DEPTH)
          return Runtime.diag(m.name, "VALIDATION_FAILED").param("input")
                        .expected("a JSON value nested at most #{Util::MAX_ARG_DEPTH} levels")
                        .remedy("Pass the input of #{m.name} without such deep nesting.").err
        end
        unless converted.is_a?(Hash)
          return Runtime.diag(m.name, "VALIDATION_FAILED").param("input").expected("an object")
                        .remedy("Pass the input of #{m.name} as one object.").err
        end

        keys = m.input_fields.to_h { |key, name, _| [name, key] }
        effective = converted.to_h { |k, v| [keys.fetch(k, k), v] }
        m.added.each do |key, schema|
          effective[key] = schema["default"] if !effective.key?(key) && schema.is_a?(Hash) && schema.key?("default")
        end
        effective
      end

      # [[step, op], ...] and the strictest safety, or an Err.
      def macro_plan(m)
        safety = m.safety
        steps = []
        m.steps.each_with_index do |step, i|
          op = operation(step["operation"].to_s)
          return macro_invalid(m.name, "Step #{i + 1} of #{m.name} names #{step['operation']}, which is not registered with the client.") unless op

          safety = op.agent["safety"] if SAFETY_RANK.fetch(op.agent["safety"], 0) > SAFETY_RANK.fetch(safety, 0)
          steps << [step, op]
        end
        [steps, safety]
      end

      # A step's arguments keyed by the step operation's argument names.
      def step_args(m, step, op, scope, pending)
        source = step["args"].nil? ? {} : step["args"]
        evaluated = pending ? Expr.evaluate_dry(source, scope, pending) : Expr.evaluate(source, scope)
        evaluated = {} if UNSET.equal?(evaluated) || evaluated.nil?
        evaluated = Json.copy(evaluated)
        return evaluated unless evaluated.is_a?(Hash)

        m.added.each_key { |key| evaluated.delete(key) } if op.id == m.extends
        evaluated.to_h { |k, v| [op.name_of_key(k), v] }
      end

      def page_limit(step) = Util.bounded(step["max_pages"], MACRO_PAGE_LIMIT, 1, 10_000).floor

      def poll_numbers(step, scope)
        budget = Util.bounded(Expr.evaluate(step["budget_ms"], scope), DEFAULT_MACRO_BUDGET_MS, 0, Util::MAX_SAFE_INTEGER)
        interval = Util.bounded(step["interval_ms"], DEFAULT_POLL_INTERVAL_MS, 0, Util::MAX_SAFE_INTEGER)
        [budget, interval]
      end

      def preview_macro_with(m, input, opts)
        name = m.name
        plan = macro_plan(m)
        return plan if plan.is_a?(Err)

        steps, safety = plan
        effective = macro_input(m, input)
        return effective if effective.is_a?(Err)
        return macro_invalid(name, "#{name} has no steps; regenerate the SDK.") if steps.empty?

        rest = opts.with(confirm: nil, verify: false)
        scope = { "input" => effective }
        pending = []
        previews = []
        effects = []
        effects << m.summary unless m.summary.empty?
        key_used = false
        steps.each_with_index do |(step, op), index|
          where = "step #{index + 1} of #{name}: #{op.id}"
          args = step_args(m, step, op, scope, pending)
          step_opts = rest
          if !uses_key?(op) || key_used
            step_opts = step_opts.with(idempotency_key: nil)
          elsif opts.key?
            key_used = true
          end
          request = nil
          if args.is_a?(Hash)
            unrenderable = op.body && %w[bytes multipart].include?(op.body.encoding) && Helpers.placeholder?(args)
            unless unrenderable
              p = prepare(op, args, step_opts, :macro_preview, nil, nil)
              if p.is_a?(Err)
                d = p.error.with(operation: name, remediation: "#{p.error.remediation} (#{where})")
                return Err.new(error: d, partial: p.partial)
              end
              request = RenderedRequest.new(http_method: p.http_method, url: Helpers.unescape_placeholders(p.display_url, args),
                                            headers: p.headers.to_h(true), body: p.body.display)
            end
          elsif !Helpers.placeholder?(args)
            return Runtime.diag(name, "VALIDATION_FAILED").param("input").expected("an object")
                          .remedy("Step #{index + 1} of #{name} does not evaluate to an argument object.").err
          end
          own = []
          conf = op.agent["confirmation"]
          own << Helpers.interpolate(conf["message"], args.is_a?(Hash) ? args : {}, op) if conf && !conf["message"].to_s.empty?
          case step["kind"]
          when "poll"
            until_text = step["until"] ? Expr.describe(step["until"]) : ""
            budget, interval = poll_numbers(step, scope)
            own << "Repeats #{op.id} every #{Json.number_text(interval.to_f)} ms#{until_text.empty? ? '' : " until #{until_text}"}, for at most #{Json.number_text(budget.to_f)} ms."
          when "paginate"
            own << "Reads up to #{page_limit(step)} pages of #{op.id}."
          end
          own << op.agent["remediation_note"] unless op.agent["remediation_note"].to_s.empty?
          effects << "Step #{index + 1}: #{step['kind']} #{op.id} (#{op.agent['safety']})."
          effects.concat(own)
          previews << MacroStepPreview.new(step: index + 1, kind: step["kind"], operation: op.id, as: step["as"],
                                           safety: op.agent["safety"], request: request, effects: own)
          pending << step["as"] if step["as"]
        end
        effects << "The result contains values the API shows only once; store them immediately." if m.shown_once
        unless previews.first.request
          return Runtime.diag(name, "VALIDATION_FAILED").param("input").expected("an object")
                        .remedy("Step 1 of #{name} cannot be rendered.").err
        end

        mutating = safety != "read_only"
        result = PreviewResult.new(
          operation: name, safety: safety, request: previews.first.request, effects: effects,
          confirmation_token: mutating ? Keys.issue_token(@confirmation_key, "macro:#{name}", effective, now) : nil,
          expires_in_ms: mutating ? Keys::TTL_MS : nil, server_preview: UNSET, steps: previews
        )
        Ok.new(value: result, meta: ResponseMeta.local)
      end

      # Running the step again (by rerunning its macro with the same input and
      # options) answers the first result instead of applying it twice.
      def rerun_protected?(op, opts)
        return true if %w[content_identity content_hash auto].include?(op.agent["idempotency"]["policy"])
        return false unless opts.key?

        op.agent["idempotency"]["policy"] == "caller_owned" || op.params.any? { |p| p.role == "idempotency_key" }
      end

      # The replay protection a macro's token is bound to: the run's key when
      # a step sends it and every mutating step is protected against a rerun.
      def macro_bind(steps, opts)
        return nil unless opts.key?

        sent = false
        steps.each do |_, op|
          step_opts = opts
          if !uses_key?(op) || sent
            step_opts = opts.with(idempotency_key: nil)
          else
            sent = true
          end
          return nil if op.mutation? && !rerun_protected?(op, step_opts)
        end
        sent ? opts.idempotency_key : nil
      end

      def run_macro_with(m, input, opts)
        name = m.name
        plan = macro_plan(m)
        return plan if plan.is_a?(Err)

        steps, safety = plan
        effective = macro_input(m, input)
        return effective if effective.is_a?(Err)

        err = confirmed(name, "macro:#{name}", "the macro's preview(...)", safety, effective, opts)
        return err if err

        claim = MacroClaim.new(name, opts.token, macro_bind(steps, opts))
        scope = { "input" => effective }
        completed = []
        produced = {}
        unprotected = []
        key_used = false
        meta = ResponseMeta.local
        steps.each_with_index do |(step, op), index|
          args = step_args(m, step, op, scope, nil)
          unless args.is_a?(Hash)
            return Runtime.diag(name, "VALIDATION_FAILED").param("input").expected("an object")
                          .remedy("Step #{index + 1} of #{name} does not evaluate to an argument object.").err
          end
          step_opts = opts.with(confirm: nil, verify: false)
          if !uses_key?(op) || key_used
            step_opts = step_opts.with(idempotency_key: nil)
          elsif opts.key?
            key_used = true
          end
          value = UNSET
          failure = nil
          case step["kind"]
          when "poll"
            budget, interval = poll_numbers(step, scope)
            until_ = step["until"].is_a?(Hash) ? step["until"] : {}
            polled = poll_with(op, args, until_, interval, budget, step_opts, claim)
            if polled.is_a?(Err)
              failure = polled
            else
              value = polled.value["timed_out"] ? nil : polled.value["body"]
              meta = polled.meta
            end
          when "paginate"
            items = []
            limit = page_limit(step)
            pages = 0
            state = new_page_state(args)
            loop do
              page = page_step(op, state, step_opts, claim)
              break if page.nil?

              if page.is_a?(Err)
                failure = page
                break
              end
              items.concat(page.value.items)
              meta = page.meta
              pages += 1
              break if pages >= limit
            end
            value = items
          else
            response = call_with(op, args, step_opts, nil, claim)
            if response.is_a?(Err)
              failure = response
            else
              value = response.raw.nil? ? UNSET : response.raw
              meta = response.meta
            end
          end
          if failure
            d = failure.error
            produced[step["as"]] = failure.partial if failure.partial? && step["as"]
            done = completed.empty? ? "no step completed before it" : "completed before it: #{completed.join(', ')}"
            remediation = "#{d.remediation} Macro #{name} stopped at step #{index + 1} of #{steps.length} (#{op.id}); #{done}."
            retryable = d.retryable
            next_action = d.next_action
            unless produced.empty?
              once = m.shown_once ? ", including values the API shows only once: store them now" : ""
              remediation += " The result's partial holds the completed steps' results (#{produced.keys.join(', ')})#{once}."
            end
            unless unprotected.empty?
              remediation += " Do not run #{name} again: #{unprotected.join(', ')} already took effect and has no replay protection."
              retryable = "after_remediation" if %w[after_delay same_key_only].include?(retryable)
              finish = "Finish #{name} without rerunning it: call #{op.id} yourself with the values from the result's partial."
              next_action = next_action ? "#{next_action} #{finish}" : finish
            end
            d = d.with(operation: name, remediation: remediation, retryable: retryable, next_action: next_action)
            return Err.new(error: d, partial: produced.empty? ? UNSET : produced)
          end
          completed << op.id
          if step["as"]
            if UNSET.equal?(value)
              scope.delete(step["as"])
              produced[step["as"]] = nil
            else
              scope[step["as"]] = value
              produced[step["as"]] = value
            end
          end
          unprotected << op.id if op.mutation? && !rerun_protected?(op, step_opts)
        end
        output = Expr.evaluate(m.output, scope)
        output = nil if UNSET.equal?(output)
        Ok.new(value: output, meta: meta)
      end
    end
  end
end
