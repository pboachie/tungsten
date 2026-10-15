# frozen_string_literal: true
# SPDX-License-Identifier: Apache-2.0

require "json"
require "fileutils"
require "monitor"
require "securerandom"
require "socket"

module Tungsten
  module Runtime
    # Keeps the idempotency key of each logical call for the life of the
    # process, so a retry of the same intent reuses it. Any object with
    # `get(scope, logical_id)` and `put(scope, logical_id, key)` can replace
    # it (ClientOptions `idempotency_store`). Persistent custom stores may add
    # `reserve(scope, logical_id, candidate)` to allocate keys atomically.
    class MemoryIdempotencyStore
      def initialize
        @keys = {}
        @lock = Mutex.new
      end

      # The key stored for a logical call, or nil.
      def get(scope, logical_id) = @lock.synchronize { @keys[[scope, logical_id]] }

      def put(scope, logical_id, key)
        @lock.synchronize { @keys[[scope, logical_id]] = key }
      end

      def reserve(scope, logical_id, candidate)
        @lock.synchronize { @keys[[scope, logical_id]] ||= candidate }
      end
    end

    # Shared version-1 JSON store, compatible with the TypeScript, Python, Go
    # and Rust runtimes. Atomic reserve waits up to two seconds for <path>.lock;
    # another writer's lease is never reclaimed, even if its PID is no longer
    # alive. Remove abandoned leases by hand after checking their owner.
    # Writes use an exclusive mode-0600 temporary file and atomic rename. An
    # unreadable or malformed existing file raises instead of issuing a new key.
    class FileIdempotencyStore
      LEASE_WAIT = 2.0
      @file_locks = {}
      @file_locks_guard = Mutex.new

      class << self
        def file_lock(path)
          @file_locks_guard.synchronize { @file_locks[path] ||= Mutex.new }
        end
      end

      attr_reader :path

      def initialize(path)
        @path = File.expand_path(path.to_s).freeze
        @lock = Monitor.new
      end

      def get(scope, logical_id)
        @lock.synchronize { read_keys.dig(scope, logical_id) }
      end

      def put(scope, logical_id, key)
        write(scope, logical_id, key)
      end

      # Atomically choose one candidate across clients and processes. get/put
      # remain supported for legacy custom stores; persistent callers should
      # use reserve when allocating an automatic key.
      def reserve(scope, logical_id, candidate)
        FileIdempotencyStore.file_lock(@path).synchronize do
          @lock.synchronize do
            FileUtils.mkdir_p(File.dirname(@path))
            lease = "#{@path}.lock"
            token = acquire_lease(lease)
            begin
              existing = get(scope, logical_id)
              return existing if existing.is_a?(String) && !existing.empty?

              write(scope, logical_id, candidate) do
                unless owns_lease?(lease, token)
                  raise IOError, "the idempotency store lease #{lease} is no longer this writer's; the key was not stored"
                end
              end
              candidate
            ensure
              release_lease(lease, token)
            end
          end
        end
      end

      private

      def read_keys
        begin
          text = File.read(@path, encoding: Encoding::UTF_8)
        rescue Errno::ENOENT
          return {}
        end
        begin
          data = JSON.parse(text)
        rescue JSON::ParserError, EncodingError, ArgumentError
          raise IOError, "the idempotency store file is not valid JSON", cause: nil
        end
        valid = data.is_a?(Hash) && data["version"] == 1 && data["keys"].is_a?(Hash) &&
                data["keys"].all? do |scope, values|
                  scope.is_a?(String) && values.is_a?(Hash) &&
                    values.all? { |id, key| id.is_a?(String) && key.is_a?(String) }
                end
        raise IOError, "the idempotency store file has an unknown format" unless valid

        data["keys"]
      end

      # A same-directory, exclusive temporary file commits only after its
      # contents are flushed and the reservation's owner token is rechecked.
      def write(scope, logical_id, key)
        unless [scope, logical_id, key].all? { |value| value.is_a?(String) }
          raise ArgumentError, "idempotency scope, logical id and key must be strings"
        end
        @lock.synchronize do
          keys = read_keys
          (keys[scope] ||= {})[logical_id] = key
          ordered = keys.keys.sort.to_h do |name|
            [name, keys[name].keys.sort.to_h { |id| [id, keys[name][id]] }]
          end
          text = "#{JSON.pretty_generate({ "version" => 1, "keys" => ordered })}\n"
          FileUtils.mkdir_p(File.dirname(@path))
          temporary = "#{@path}.#{SecureRandom.hex(6)}.tmp"
          created = false
          begin
            File.open(temporary, File::WRONLY | File::CREAT | File::EXCL, 0o600) do |file|
              created = true
              file.write(text)
              file.flush
              file.fsync
            end
            yield if block_given?
            File.rename(temporary, @path)
          ensure
            begin
              File.unlink(temporary) if created
            rescue SystemCallError
              # A completed rename or failed cleanup does not change the outcome.
            end
          end
        end
      end

      def read_owner(lease)
        owner = JSON.parse(File.read(lease, encoding: Encoding::UTF_8))
        return nil unless owner.is_a?(Hash) && owner["version"] == 1 &&
                          owner["host"].is_a?(String) && owner["pid"].is_a?(Integer) &&
                          owner["pid"].positive? && owner["pid"] <= 9_007_199_254_740_991 &&
                          owner["token"].is_a?(String) && !owner["token"].empty?

        owner
      rescue SystemCallError, IOError, JSON::ParserError, EncodingError, ArgumentError
        nil
      end

      def owns_lease?(lease, token) = read_owner(lease)&.fetch("token") == token

      def acquire_lease(lease)
        token = SecureRandom.hex(16)
        host = begin
          Socket.gethostname
        rescue SocketError, SystemCallError
          ""
        end
        mine = "#{JSON.generate({ "version" => 1, "host" => host, "pid" => Process.pid, "token" => token })}\n"
        deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + LEASE_WAIT
        delay = 0.005
        loop do
          begin
            File.open(lease, File::WRONLY | File::CREAT | File::EXCL, 0o600) { |file| file.write(mine) }
            return token
          rescue Errno::EEXIST
            # Never reclaim a live, dead or malformed lease from another writer.
          rescue StandardError
            release_lease(lease, token)
            raise
          end
          left = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
          if left <= 0
            owner = read_owner(lease)
            detail = owner ? ", process #{owner["pid"]} on #{JSON.generate(owner["host"])}" : ""
            remedy = owner ? "; try again, or remove the lease once that process no longer runs" : "; try again"
            raise IOError, "the idempotency store is locked by another writer (#{lease}#{detail})#{remedy}"
          end
          sleep([delay, left].min)
          delay = [delay * 2, 0.1].min
        end
      end

      def release_lease(lease, token)
        File.unlink(lease) if owns_lease?(lease, token)
      rescue SystemCallError, IOError
        # The reservation outcome is already decided; only our token may release.
      end
    end

    # Idempotency key formats and confirmation tokens.
    module Keys
      module_function

      def format_kind(format)
        case format.to_s.downcase.gsub(/[^a-z0-9]/, "")
        when "uuidv4", "uuid4" then "uuid_v4"
        when "uuid" then "uuid"
        when "asciitoken128" then "ascii_token_128"
        else "token"
        end
      end

      # What a policy's key format expects, in words.
      def format_description(format)
        case format_kind(format)
        when "uuid_v4" then "UUIDv4 string"
        when "uuid" then "UUID string"
        when "ascii_token_128" then "1-128 ASCII letters, digits, dots, underscores or hyphens"
        else "a non-empty printable ASCII string of at most 255 characters"
        end
      end

      def uuid?(key, v4_only)
        version = v4_only ? "4" : "[1-8]"
        key.is_a?(String) && key.match?(/\A\h{8}-\h{4}-#{version}\h{3}-[89abAB]\h{3}-\h{12}\z/)
      end

      # The expected format when the key does not match it, else nil.
      def check_format(key, format)
        valid =
          case format_kind(format)
          when "uuid_v4" then uuid?(key, true)
          when "uuid" then uuid?(key, false)
          when "ascii_token_128" then key.is_a?(String) && key.match?(/\A[A-Za-z0-9._-]{1,128}\z/)
          else key.is_a?(String) && !key.empty? && key.bytesize <= 255 && key.match?(/\A[\x21-\x7e]+\z/)
          end
        valid ? nil : format_description(format)
      end

      # The wire header of an operation's key.
      def header(op)
        h = op.agent["idempotency"]["header"]
        return h if h.is_a?(String) && !h.empty?

        op.params.find { |p| p.role == "idempotency_key" }&.wire || "Idempotency-Key"
      end

      # The lifetime of a confirmation token.
      TTL_MS = 5 * 60 * 1000
      PREFIX = "tgc1"

      # The SHA-256 of the canonical JSON of the arguments.
      def args_digest(args) = Util.sha256_hex(Json.canonical(args))

      def signature(key, operation, digest, expiry)
        Util.base64url(Util.hmac_sha256(key, "#{operation}\n#{digest}\n#{expiry}"))
      end

      # tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, operation, args digest,
      # expiry))>: valid only for the operation and the exact arguments.
      def issue_token(key, operation, args, now)
        expiry = now + TTL_MS
        "#{PREFIX}.#{expiry}.#{signature(key, operation, args_digest(args), expiry)}"
      end

      # [expiry, signature] of a token of the right shape, else nil.
      def token_expiry(token)
        m = token.to_s.match(/\A#{PREFIX}\.(\d{1,16})\.([A-Za-z0-9_-]{43})\z/o)
        return nil unless m && m[1].to_i.to_s == m[1]

        [m[1].to_i, m[2]]
      end

      # :valid, :expired, :mismatch or :malformed.
      def check_token(key, token, operation, args, now)
        parsed = token_expiry(token)
        return :malformed unless parsed

        expiry, given = parsed
        return :mismatch unless Util.timing_safe_equal?(signature(key, operation, args_digest(args), expiry), given)

        expiry > now ? :valid : :expired
      end
    end
  end
end
