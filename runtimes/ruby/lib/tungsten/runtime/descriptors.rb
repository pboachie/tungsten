# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

module Tungsten
  module Runtime
    # One parameter: `name` is its key in the arguments object (the SDK's
    # argument name), `wire` its name on the wire. Parameters with the roles
    # idempotency_key, origin, auth and constant are not arguments.
    ParamDescriptor = Data.define(:key, :name, :wire, :location, :required, :style, :explode, :role, :constant) do
      # Whether the caller passes it among the arguments.
      def argument? = %w[plain dry_run].include?(role)
    end

    # The request body: with `arg` set the whole body is that argument;
    # otherwise a JSON object of the merged `fields` ([name, wire] pairs).
    BodyDescriptor = Data.define(:media_type, :encoding, :required, :fields, :arg)

    # A declared response: `status` an Integer, `"4XX"` or `"default"`.
    ResponseDescriptor = Data.define(:status, :kind, :media_type)

    # What the runtime needs to call one operation, from the SDK descriptor
    # document v1, with every argument named by its `name`.
    class OperationDescriptor
      attr_reader :id, :http_method, :path, :params, :body, :responses, :security, :pagination, :rpc,
                  :error_code_field, :gate, :agent, :request, :response, :page_item, :stream, :summary,
                  :key_names

      # @param doc [Hash] one entry of the document's `operations`
      # @param defs [Hash] the document's `defs`
      def initialize(doc, defs)
        @id = doc["id"].to_s
        @http_method = doc["method"].to_s.upcase
        @path = doc["path"].to_s
        @params = (doc["params"] || []).map do |p|
          ParamDescriptor.new(key: p["key"].to_s, name: (p["name"] || p["key"]).to_s, wire: p["wire"].to_s,
                              location: p["in"].to_s, required: p["required"] == true, style: p["style"] || "simple",
                              explode: p["explode"] == true, role: p["role"] || "plain", constant: p["constant"])
        end
        @key_names = {}
        @params.each { |p| @key_names[p.key] = p.name }
        @body = body_of(doc["body"])
        @responses = (doc["responses"] || []).map do |r|
          ResponseDescriptor.new(status: r["status"], kind: r["kind"].to_s, media_type: r["media_type"])
        end
        @security = doc["security"] || []
        @pagination = pagination_of(doc["pagination"])
        @rpc = doc["rpc"]
        @error_code_field = doc["error_code_field"]
        status = doc["status"] || {}
        @gate = status["kind"] == "gated" ? { "env_var" => status["env_var"], "disabled_status" => status["disabled_status"] } : nil
        @agent = agent_of(doc["agent"] || {})
        @request = doc["request"] ? Schema.new(doc["request"], defs) : nil
        @response = doc["response"] ? Schema.new(doc["response"], defs) : nil
        @page_item = doc["page_item"] ? Schema.new(doc["page_item"], defs) : nil
        s = doc["stream"]
        @stream = s ? { "event" => Schema.new(s["event"], defs), "done" => s["done"], "flag" => s["flag"], "text" => s["text"] == true } : nil
        @summary = doc["summary"].to_s
      end

      # The parameters the caller supplies as arguments.
      def argument_params = @params.select(&:argument?)

      # Whether the operation can change state (it is not read-only).
      def mutation? = @agent["safety"] != "read_only"

      # The argument name of an arguments layout key, or the key itself.
      def name_of_key(key) = @key_names.fetch(key, key)

      # The argument name of a parameter given by name or wire name.
      def arg_name(key)
        p = @params.find { |x| x.name == key } || @params.find { |x| x.wire == key }
        p ? p.name : key
      end

      private

      def body_of(b)
        return nil unless b

        shape = b["shape"] || {}
        fields = []
        arg = nil
        if shape["kind"] == "merged"
          (shape["fields"] || []).each do |f|
            name = (f["name"] || f["key"]).to_s
            fields << [name, f["wire"].to_s]
            @key_names[f["key"].to_s] = name
          end
        else
          arg = (shape["name"] || shape["arg"]).to_s
          @key_names[shape["arg"].to_s] = arg
        end
        BodyDescriptor.new(media_type: b["media_type"].to_s, encoding: b["encoding"] || "json",
                           required: b["required"] == true, fields: fields, arg: arg)
      end

      def pagination_of(p)
        return nil unless p

        out = p.dup
        %w[request_param page_size_param offset_param limit_param page_param size_param].each do |k|
          out[k] = name_of_key(p[k]) if p[k]
        end
        out
      end

      def agent_of(a)
        out = a.dup
        out["safety"] ||= "read_only"
        out["idempotency"] = { "policy" => "none", "persist_required" => false }.merge(a["idempotency"] || {})
        out["preview"] = a["preview"] || { "mode" => "local" }
        out["remediation"] = a["remediation"] || {}
        out["sensitive_response_fields"] = a["sensitive_response_fields"] || []
        out["sensitive_request_fields"] = (a["sensitive_request_fields"] || []).map do |path|
          head, rest = path.to_s.split(".", 2)
          rest ? "#{name_of_key(head)}.#{rest}" : name_of_key(head)
        end
        out["shown_once"] = a["shown_once"] == true
        out
      end
    end

    # A macro in the canonical form, from the SDK descriptor document.
    MacroDescriptor = Data.define(:name, :summary, :safety, :steps, :output, :input, :sensitive_response_fields,
                                  :shown_once, :cluster) do
      # The input fields: [key, name, required].
      def input_fields = (input["fields"] || []).map { |f| [f["key"].to_s, (f["name"] || f["key"]).to_s, f["required"] == true] }

      # The operation whose arguments the input takes.
      def extends = input["extends"]

      # The added fields as JSON Schemas.
      def added = input["add"] || {}
    end

    # The SDK descriptor document of a generated SDK
    # (`tungsten-descriptors.json`, format `tungsten-sdk-descriptors/1`).
    class Descriptors
      FORMAT = "tungsten-sdk-descriptors/1"

      attr_reader :api, :operations, :macros, :defs, :target

      # Read the document at a path.
      def self.load(path) = new(Json.parse(File.read(path, mode: "rb")))

      # @param doc [Hash] the parsed document
      def initialize(doc)
        raise ArgumentError, "not an SDK descriptor document (format #{doc['format'].inspect})" unless doc.is_a?(Hash) && doc["format"] == FORMAT

        @target = doc["target"]
        @api = doc["api"] || {}
        @defs = doc["defs"] || {}
        @operations = (doc["operations"] || {}).transform_values { |o| OperationDescriptor.new(o, @defs) }
        @macros = (doc["macros"] || {}).transform_values do |m|
          MacroDescriptor.new(name: m["name"].to_s, summary: m["summary"].to_s, safety: m["safety"] || "read_only",
                              steps: m["steps"] || [], output: m["output"], input: m["input"] || {},
                              sensitive_response_fields: m["sensitive_response_fields"] || [],
                              shown_once: m["shown_once"] == true, cluster: m["cluster"])
        end
      end

      def operation(id) = @operations[id]
      def macro(name) = @macros[name]

      # A validator of the named type `id` (UNSET when the document does not
      # define it).
      def schema_of(id)
        @schemas ||= {}
        @schemas[id] ||= @defs.key?(id) ? Schema.new(@defs[id], @defs) : UNSET
      end
    end
  end
end
