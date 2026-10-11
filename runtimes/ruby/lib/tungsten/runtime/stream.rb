# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # What the consuming helpers of a stream return: the result, or when
    # the stream failed, what was gathered before the failure with the
    # failure as `error` (an Err); `reconnects` of the last event delivered.
    Folded = Data.define(:value, :error, :reconnects) do
      def ok? = error.nil?
    end

    # The server-sent events of an operation (`client.<resource>.<op>_stream`).
    # The request is sent when the enumeration starts; an error ends it
    # (`error` is then the Err). A dropped stream is reconnected with
    # `Last-Event-ID` when the call is safe to repeat, and an event whose id
    # was delivered is never delivered twice. `cancel` ends it for good,
    # without an error, from any thread.
    class EventStream
      include Enumerable

      # The connection being read and where the stream is.
      Open = Struct.new(:body, :parser, :decoder, :queue, :ended, :oversize, :meta, :delivered_count, :key, :key_header, :check,
                        :secrets, :timeout_ms, :repeatable, :reconnects, :last_id, :last_retry, :delivered, :replaying,
                        keyword_init: true)

      attr_reader :error

      def initialize(core, op, args, opts, decode)
        @core = core
        @op = op
        @args = args
        @opts = opts
        @decode = decode
        @state = :idle
        @error = nil
        @cancelled = false
        @lock = Mutex.new
        @open = nil
      end

      # The operation's descriptor.
      def operation = @op

      # End the stream for good, without an error and without a reconnect.
      def cancel
        body = @lock.synchronize do
          @cancelled = true
          @open&.body
        end
        body&.close
        nil
      end

      def cancelled? = @lock.synchronize { @cancelled }

      # Close the connection.
      def close
        @open&.body&.close
        set_open(nil)
        @state = :done
        nil
      end

      # Every event, in order; stops at the end, at a failure (`error`) or
      # after `cancel`.
      def each
        return enum_for(:each) unless block_given?

        while (event = next_event)
          yield event
        end
        self
      end

      # Read the stream to its end, calling the handler named like each
      # event (`"message"` for one without a name), else the one named `"*"`;
      # `value` is the number of events delivered.
      def on(handlers)
        count = 0
        reconnects = 0
        handlers = handlers.transform_keys(&:to_s)
        each do |event|
          reconnects = event.reconnects
          count += 1
          (handlers[event.event] || handlers["*"])&.call(event)
        end
        Folded.new(value: count, error: @error, reconnects: reconnects)
      ensure
        close
      end

      # Read the stream to its end and fold its events into one value.
      def reduce(initial)
        acc = initial
        reconnects = 0
        each do |event|
          reconnects = event.reconnects
          acc = yield(acc, event)
        end
        Folded.new(value: acc, error: @error, reconnects: reconnects)
      ensure
        close
      end

      # The first event for which the block holds (any event without a
      # block), then close the stream; `value` is nil when none did.
      def first(*)
        reconnects = 0
        each do |event|
          reconnects = event.reconnects
          return Folded.new(value: event, error: nil, reconnects: reconnects) if !block_given? || yield(event)
        end
        Folded.new(value: nil, error: @error, reconnects: reconnects)
      ensure
        close
      end

      # Every event in order and the final failure, if any: [events, Err or
      # nil]. The collection is bounded: it ends with UNEXPECTED_RESPONSE
      # once the kept events hold more than ClientOptions `max_collect_bytes`
      # of JSON or it ran longer than `max_collect_ms`.
      def collect
        started = Transport.monotonic_ms
        events = []
        size = 0
        each do |event|
          if Transport.monotonic_ms - started > @core.max_collect_ms
            close
            return [events, @core.send(:over_budget, @op, false, events.length)]
          end
          size += Json.text(event.raw).bytesize
          if size > @core.max_collect_bytes
            close
            return [events, @core.send(:over_budget, @op, true, events.length)]
          end
          events << event
        end
        [events, @error]
      end

      private

      def set_open(open)
        cancelled = @lock.synchronize do
          @open = open
          @cancelled
        end
        open.body.close if cancelled && open
      end

      def fail_with(err)
        @error = err
        close
        nil
      end

      # The next event, or nil at the end.
      def next_event
        loop do
          case @state
          when :done then return nil
          when :idle
            if cancelled?
              @state = :done
              return nil
            end
            open = @core.send(:open_stream, @op, @args, @opts, nil)
            return fail_with(open) if open.is_a?(Err)

            set_open(open)
            @state = :open
            if cancelled?
              close
              return nil
            end
            next
          end
          o = @open
          if cancelled?
            close
            return nil
          end
          unless o.queue.empty?
            ev = o.queue.shift
            item = @core.send(:accept_event, @op, o, ev, @decode)
            return fail_with(item) if item.is_a?(Err)
            if item == :done
              close
              return nil
            end
            next if item.nil?

            return item
          end
          return fail_with(@core.send(:oversize, @op, o)) if o.oversize

          why = nil
          if o.ended
            # A stream that names its terminal event has not finished without it.
            if @op.stream["done"].nil?
              close
              return nil
            end
            why = :early_end
          else
            kind, chunk = o.body.read
            if cancelled?
              close
              return nil
            end
            case kind
            when :chunk
              o.queue.concat(o.parser.push(o.decoder.decode(chunk)))
              o.oversize = o.parser.exceeded?
              next
            when :end
              o.queue.concat(o.parser.push(o.decoder.finish))
              o.queue.concat(o.parser.finish)
              o.oversize = o.parser.exceeded?
              o.ended = true
              next
            when :timeout
              return fail_with(@core.send(:interrupted, @op, o, nil)) unless @core.idle_timeout_ms

              why = :idle
            else
              why = :lost
            end
          end
          unless @core.send(:may_reconnect?, o)
            # Without reconnects a clean end stays a clean end.
            if why == :early_end && o.reconnects.zero?
              close
              return nil
            end
            return fail_with(@core.send(:interrupted, @op, o, why))
          end
          fresh = @core.send(:reconnect, @op, @args, @opts, o)
          if cancelled?
            fresh.body.close if fresh.is_a?(Open)
            close
            return nil
          end
          return fail_with(fresh) if fresh.is_a?(Err)

          set_open(fresh)
        end
      end
    end

    class ClientCore
      # The wait before a reconnect when the server sent no retry value.
      DEFAULT_RECONNECT_MS = 250

      # The server-sent events of an operation (an id or a descriptor).
      # `decode` turns each event's JSON data into the SDK's type.
      #
      # @return [EventStream]
      def stream(op, args = {}, opts = nil, decode: nil)
        d = op.is_a?(OperationDescriptor) ? op : operation(op.to_s)
        unless d&.stream
          id = op.is_a?(OperationDescriptor) ? op.id : op.to_s
          err = Runtime.diag(id, "VALIDATION_FAILED").param("operation").expected("an operation with an event stream")
                       .remedy("#{id} has no event stream (its success response is not text/event-stream); use call instead. Nothing was sent.").err
          return FailedStream.new(err)
        end
        o = begin
          CallOptions.from(opts)
        rescue ArgumentError => e
          return FailedStream.new(Runtime.diag(d.id, "VALIDATION_FAILED").param("opts").expected("call options")
                                         .remedy("Pass the call options as a Hash of idempotency_key, confirm, timeout_ms, verify and headers (#{e.message}).").err)
        end
        EventStream.new(self, d, args, o, decode)
      end

      # A stream that fails before it starts.
      class FailedStream < EventStream
        def initialize(err)
          super(nil, nil, nil, nil, nil)
          @error = err
          @state = :done
        end
      end

      private

      # The stream's request flag set to true, in a merged body field or
      # inside an object body argument.
      def with_stream_flag(op, args)
        flag = op.stream["flag"]
        return args if flag.nil? || op.body.nil? || !args.is_a?(Hash)

        out = args.dup
        if op.body.arg.nil?
          field = op.body.fields.find { |_, wire| wire == flag }
          out[field[0]] = true if field
        elsif args[op.body.arg].is_a?(Hash)
          out[op.body.arg] = args[op.body.arg].merge(flag => true)
        end
        out
      end

      def open_stream(op, args, opts, resume_from)
        headers = {}
        accept = ""
        [[false, @headers], [true, (opts.headers || {}).to_h.transform_keys(&:to_s)]].each do |own, layer|
          layer.each do |name, value|
            if name.casecmp?("accept")
              accept = value.to_s
            elsif own
              headers[name] = value
            end
          end
        end
        headers["Accept"] = Util.merge_accept(accept)
        if resume_from
          headers.delete_if { |name, _| name.casecmp?("last-event-id") }
          headers["Last-Event-ID"] = resume_from
        end
        stream_opts = opts.with(headers: headers)
        converted =
          begin
            Json.from_ruby(args.nil? ? {} : args)
          rescue ArgumentError
            args
          end
        p = prepare(op, with_stream_flag(op, converted), stream_opts, :call, nil, nil)
        return p if p.is_a?(Err)

        result = send_with(p, stream_opts, true)
        return result if result.is_a?(Err)

        if result.is_a?(Ok)
          type = result.meta.headers["content-type"]
          after = op.mutation? ? " The call took effect; do not repeat it." : ""
          return Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(result.meta.status).request_id(result.meta.request_id)
                        .param("response").received(type ? Util.envelope_value(type, false) : nil)
                        .expected("a text/event-stream body")
                        .remedy("The success response is not an event stream (Content-Type #{type || 'missing'}).#{after}")
                        .retrying("never").attempts(result.meta.attempts).err
        end
        _, meta, body = result
        check = op.mutation? && !Classify.replay_protection?(op, !p.key.nil?) ? outcome_check(op, p.args) : nil
        EventStream::Open.new(body: body, parser: SseParser.new(@max_event_bytes), decoder: Utf8Decoder.new, queue: [],
                              ended: false, oversize: false, meta: meta, delivered_count: 0, key: p.key, key_header: p.key_header,
                              check: check, secrets: p.secrets.dup, timeout_ms: attempt_timeout(stream_opts),
                              repeatable: !op.mutation? || Classify.replay_protection?(op, !p.key.nil?), reconnects: 0,
                              last_id: nil, last_retry: nil, delivered: {}, replaying: false)
      end

      # A dropped stream may be reconnected when the call is safe to repeat,
      # reconnects are left, and an event id to resume from is known (or no
      # event was delivered yet).
      def may_reconnect?(o)
        o.repeatable && o.reconnects < @max_reconnects &&
          (o.delivered_count.zero? || (!o.last_id.nil? && Serialize.valid_header_value?(o.last_id)))
      end

      # Wait as the server asked (capped, shortened by up to 25 % at random),
      # then send the request again with Last-Event-ID.
      def reconnect(op, args, opts, old)
        old.body.close
        ceiling = [old.last_retry || DEFAULT_RECONNECT_MS, @reconnect_max_ms].min.to_f
        sleep([ceiling - (random * 0.25 * ceiling), 0].max / 1000.0)
        fresh = open_stream(op, args, opts, old.last_id)
        return fresh if fresh.is_a?(Err)

        fresh.delivered_count = old.delivered_count
        fresh.reconnects = old.reconnects + 1
        fresh.last_id = old.last_id
        fresh.last_retry = old.last_retry
        fresh.delivered = old.delivered
        fresh.replaying = true
        fresh
      end

      def plural(n, one, many) = n == 1 ? one : many

      # The final error of a stream that broke after `o.delivered_count` events; `why`
      # is :lost, :idle, :early_end, or nil for a silence past the attempt
      # timeout.
      def interrupted(op, o, why)
        after = "after #{o.delivered_count} #{plural(o.delivered_count, 'event', 'events')}"
        retried =
          case o.reconnects
          when 0 then ""
          when 1 then " (1 reconnect made)"
          else " (#{o.reconnects} reconnects made)"
          end
        idle = why == :idle
        code = idle ? "STREAM_IDLE" : nil
        status = o.meta.status
        ctx = Classify::CallContext.new(api: @api, op: op, key: o.key, key_header: o.key_header, attempts: o.meta.attempts, check: o.check)
        d =
          if why.nil? || idle
            silence = "No event arrived within #{o.timeout_ms} ms #{after}"
            option = "timeout_ms"
            if idle
              silence = "No bytes arrived within #{@idle_timeout_ms} ms #{after}#{retried}"
              option = "idle_timeout_ms"
            end
            cause = "#{silence}; the stream was abandoned."
            if op.mutation?
              Classify.outcome_unknown(ctx, cause, http_status: status, code: code, request_id: o.meta.request_id)
            else
              Runtime.diag(op.id, "UPSTREAM_UNAVAILABLE").status(status).code(code).request_id(o.meta.request_id)
                     .remedy("#{cause} This read has no side effects; call again later or with a larger #{option}.")
                     .attempts(o.meta.attempts).build
            end
          else
            cause = "The event stream was cut off #{after}#{retried}: the connection failed before it ended."
            if op.mutation?
              Classify.outcome_unknown(ctx, cause, http_status: status, request_id: o.meta.request_id)
            else
              Runtime.diag(op.id, "TRANSPORT_FAILED").status(status).request_id(o.meta.request_id)
                     .remedy("#{cause} This read has no side effects; call again.").attempts(o.meta.attempts).build
            end
          end
        Err.new(error: Util.scrub_diagnostic(d, o.secrets))
      end

      def over_budget(op, bytes, kept)
        what = bytes ? "at most #{@max_collect_bytes} bytes of events" : "at most #{@max_collect_ms} ms to collect the events"
        after = op.mutation? ? " The call took effect; do not repeat it." : ""
        Runtime.diag(op.id, "UNEXPECTED_RESPONSE").param("events").expected(what)
               .remedy("The stream was abandoned after #{kept} #{plural(kept, 'event', 'events')} (#{what}; max_collect_bytes and max_collect_ms).#{after} " \
                       "The events collected so far precede this error.")
               .retrying("never").err
      end

      def oversize(op, o)
        limit = @max_event_bytes
        after = op.mutation? ? " The call took effect; do not repeat it." : ""
        d = Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(o.meta.status).request_id(o.meta.request_id)
                   .param("events[#{o.delivered_count}]").expected("an event of at most #{limit} bytes")
                   .remedy("Event #{o.delivered_count} of the stream is larger than the limit of #{limit} bytes (max_event_bytes); the stream was abandoned.#{after} " \
                           "Events before it were delivered. Raise max_event_bytes if the server sends events this large on purpose.")
                   .retrying("never").attempts(o.meta.attempts).build
        Err.new(error: Util.scrub_diagnostic(d, o.secrets))
      end

      # Decode, check and number one event: a StreamEvent, :done for the done
      # sentinel, nil for a replayed event, or an Err.
      def accept_event(op, o, ev, decode)
        return :done if !op.stream["done"].nil? && op.stream["done"] == ev.data

        if o.replaying
          return nil if !ev.id.nil? && o.delivered[ev.id]

          o.replaying = false
        end
        index = o.delivered_count
        after_effect = op.mutation? ? " The call took effect; do not repeat it." : ""
        failure = lambda do |b|
          Err.new(error: Util.scrub_diagnostic(b.status(o.meta.status).request_id(o.meta.request_id).retrying("never")
                                                 .attempts(o.meta.attempts).build, o.secrets))
        end
        value = Json.try_parse(ev.data)
        if UNSET.equal?(value)
          return failure.call(Runtime.diag(op.id, "UNEXPECTED_RESPONSE").param("events[#{index}]")
                                     .received(Util.envelope_value(ev.data, false)).expected("JSON in the data of every event")
                                     .remedy("Event #{index} of the stream (event \"#{ev.event}\") does not carry JSON in its data.#{after_effect} Events before it were delivered."))
        end
        if @validate_responses != "off" && op.stream["event"]
          issues = judge(op.stream["event"], value)
          unless issues.empty?
            issue = issues.first
            path_text = Runtime.issue_path(issue.path)
            redacted = Util.redact_paths(value, op.agent["sensitive_response_fields"])
            received = Util.get_path(redacted, issue.path)
            sensitive = issue.path.any? { |s| s.is_a?(String) && Util.looks_sensitive?(s) }
            e = failure.call(Runtime.diag(op.id, "UNEXPECTED_RESPONSE").param("events[#{index}]#{path_text}")
                                    .received(Util.envelope_value(received, sensitive)).expected(issue.message)
                                    .remedy("Event #{index} of the stream (event \"#{ev.event}\") does not match the API description at events[#{index}]#{path_text} (#{issue.message}).#{after_effect}"))
            return e if @validate_responses == "strict"

            emit(e.error)
          end
        end
        o.delivered_count += 1
        unless ev.id.nil?
          o.delivered[ev.id] = true
          o.last_id = ev.id
        end
        o.last_retry = ev.retry_ms unless ev.retry_ms.nil?
        typed = decode ? decode.call(value) : value
        StreamEvent.new(value: typed, raw: value, event: ev.event, id: o.last_id, retry_ms: o.last_retry, meta: o.meta,
                        reconnects: o.reconnects)
      end
    end
  end
end
