# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # The base of every generated `Dispatch`: calls by operation id or macro
    # name with JSON arguments keyed by the SDK's argument names, for
    # drivers that only know ids and JSON (the contract suite, tool
    # servers). Arguments that fit a typed method (every required name
    # given, no unknown name) run it; any others go to the core as they are,
    # so pre-flight validation reports them. Results are the typed methods'
    # (`raw` is the wire value).
    class Dispatcher
      # @param client [Object] the generated client
      # @param operations [Hash] id => [required names, every name, call]
      def initialize(client, operations:, previews:, pages:, streams:, macros:)
        @client = client
        @core = client.core
        @operations = operations
        @previews = previews
        @pages = pages
        @streams = streams
        @macros = macros
      end

      attr_reader :core

      # Every callable operation's id, in IR order.
      def operations = @operations.keys

      # Every macro's name, in IR order.
      def macros = @macros.keys

      # Call an operation.
      # @return [Ok, Err]
      def invoke(operation, args = {}, opts = nil)
        run(@operations, operation, args, opts) { @core.call(operation, args, opts) }
      end

      # Preview an operation.
      def preview_operation(operation, args = {}, opts = nil)
        run(@previews, operation, args, opts) { @core.preview(operation, args, opts) }
      end

      # Every page of a paginated operation: an Enumerator of Ok and, last,
      # Err.
      def pages(operation, args = {}, opts = nil)
        unless @core.operation(operation)&.pagination
          return [Runtime.diag(operation, "VALIDATION_FAILED").param("operation").expected("a paginated operation")
                         .remedy("#{operation} is not paginated; use invoke instead. Nothing was sent.").err].each
        end
        run(@pages, operation, args, opts) { @core.pages(operation, args, opts) }
      end

      # The event stream of an operation.
      # @return [EventStream]
      def stream(operation, args = {}, opts = nil)
        run(@streams, operation, args, opts) { @core.stream(operation, args, opts) }
      end

      # Run a macro.
      def run_macro(name, input = {}, opts = nil)
        run(@macros, name, input, opts) { @core.run_macro(name, input, opts) }
      end

      # Preview a macro.
      def preview_macro(name, input = {}, opts = nil) = @core.preview_macro(name, input, opts)

      private

      def run(table, name, args, opts)
        entry = table[name]
        return yield unless entry && args.is_a?(Hash)

        required, every, call = entry
        given = args.keys.map(&:to_s)
        return yield unless (given - every).empty? && (required - given).empty?

        call.call(@client, args.to_h { |k, v| [k.to_sym, v] }, opts)
      end
    end
  end
end
