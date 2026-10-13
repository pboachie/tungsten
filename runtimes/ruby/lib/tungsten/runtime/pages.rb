# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    class ClientCore
      # Where a pagination is: the arguments of the next page, the URL of a
      # Link header page, the last cursor seen, and an error to report next.
      PageState = Struct.new(:args, :override_url, :previous_cursor, :done, :pending_error)

      # Every page of a paginated operation, fetched as the enumeration
      # proceeds: each item is Ok (whose value is a Page) or, last, Err.
      #
      # @return [Enumerator<Ok, Err>]
      def pages(op, args = {}, opts = nil)
        Enumerator.new do |y|
          result = with_options(op, opts) do |d, o|
            state = new_page_state(args)
            loop do
              page = page_step(d, state, o, nil)
              break if page.nil?

              y << page
              break if page.is_a?(Err)
            end
            nil
          end
          y << result if result.is_a?(Err)
        end
      end

      private

      def new_page_state(args)
        converted =
          begin
            Json.from_ruby(args.nil? ? {} : args)
          rescue ArgumentError
            args
          end
        PageState.new(converted.is_a?(Hash) ? converted.dup : converted, nil, UNSET, false, nil)
      end

      def set_arg(args, key, value)
        args[key] = value if args.is_a?(Hash)
      end

      def arg_at(args, key) = args.is_a?(Hash) ? args.fetch(key, UNSET) : UNSET

      # The next page (Ok or Err), or nil after the last one.
      def page_step(op, state, opts, claim)
        if state.pending_error
          err = state.pending_error
          state.pending_error = nil
          state.done = true
          return err
        end
        return nil if state.done

        response = call_with(op, state.args, opts, state.override_url, claim)
        if response.is_a?(Err)
          state.done = true
          return response
        end
        body = response.raw.nil? ? UNSET : response.raw
        pg = op.pagination
        items_field = pg ? pg["items_field"].to_s : ""
        found = items_field.empty? ? body : Util.get_path_str(body, items_field)
        items = found.is_a?(Array) ? found.dup : []
        next_page = nil
        case pg && pg["style"]
        when "cursor"
          usable = ->(v) { !UNSET.equal?(v) && !v.nil? && v != "" }
          cursor = UNSET
          if pg["response_field"].to_s != ""
            v = Util.get_path_str(body, pg["response_field"])
            cursor = v if usable.call(v)
          end
          if UNSET.equal?(cursor) && pg["cursor_item_field"].to_s != "" && !items.empty?
            v = Util.get_path_str(items.last, pg["cursor_item_field"])
            cursor = v if usable.call(v)
          end
          finished = false
          if pg["has_more_field"].to_s != ""
            v = Util.get_path_str(body, pg["has_more_field"])
            finished = v == false
          end
          repeated = !UNSET.equal?(state.previous_cursor) && !UNSET.equal?(cursor) &&
                     Json.canonical(state.previous_cursor) == Json.canonical(cursor)
          if !UNSET.equal?(cursor) && !finished && !repeated
            next_page = cursor
            state.previous_cursor = cursor
            set_arg(state.args, op.arg_name(pg["request_param"]), cursor)
          end
        when "offset"
          offset_key = op.arg_name(pg["offset_param"])
          limit = arg_at(state.args, op.arg_name(pg["limit_param"]))
          offset = Util.bounded(arg_at(state.args, offset_key), 0, 0, Util::MAX_SAFE_INTEGER)
          short = limit.is_a?(Numeric) && items.length < limit
          if !items.empty? && !short
            next_page = Util.as_number(offset + items.length)
            set_arg(state.args, offset_key, next_page)
          end
        when "page"
          page_key = op.arg_name(pg["page_param"])
          size = arg_at(state.args, op.arg_name(pg["size_param"]))
          page = Util.bounded(arg_at(state.args, page_key), 1, 0, Util::MAX_SAFE_INTEGER)
          short = size.is_a?(Numeric) && items.length < size
          if !items.empty? && !short
            next_page = Util.as_number(page + 1)
            set_arg(state.args, page_key, next_page)
          end
        when "link_header"
          link = Transport.next_link(response.meta.headers["link"])
          if link
            target = link_target(state.override_url || @base_url, link)
            if target
              state.override_url = target
              next_page = target
            else
              state.pending_error = Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(response.meta.status)
                                           .request_id(response.meta.request_id)
                                           .remedy("The next-page Link header points to another origin; it is not followed so credentials never leave the API's host. Stop paginating here.")
                                           .attempts(response.meta.attempts).err
            end
          end
        end
        err = check_items(op, items_field, items, response)
        if err
          state.done = true
          return err
        end
        state.done = true if next_page.nil?
        Ok.new(value: Page.new(items: items, body: UNSET.equal?(body) ? nil : body, next_page: next_page), meta: response.meta)
      end

      def link_target(base, link)
        origin = URI.parse(base)
        return nil if origin.scheme.nil?

        resolved = origin.merge(link)
        same_origin?(origin, resolved) ? resolved.to_s : nil
      rescue URI::Error
        nil
      end

      # Page items checked with the operation's page item validator.
      def check_items(op, items_field, items, response)
        return nil if op.page_item.nil? || @validate_responses == "off"

        items.each_with_index do |item, index|
          issues = judge(op.page_item, item)
          next if issues.empty?

          issue = issues.first
          path = "response"
          path += ".#{items_field}" unless items_field.empty?
          path += "[#{index}]"
          issue.path.each { |seg| path += seg.is_a?(Integer) ? "[#{seg}]" : ".#{seg}" }
          d = Runtime.diag(op.id, "UNEXPECTED_RESPONSE").status(response.meta.status).request_id(response.meta.request_id)
                     .param(path).expected(issue.message)
                     .remedy("A page item does not match the API description at #{path} (#{issue.message}).")
                     .attempts(response.meta.attempts).build
          return Err.new(error: d) if @validate_responses == "strict"

          emit(d)
          return nil
        end
        nil
      end
    end
  end
end
