// SPDX-License-Identifier: Apache-2.0
import type {
  ApiDescriptor,
  CallOptions,
  ClientCoreApi,
  ClientOptions,
  Diagnostic,
  OperationDescriptor,
  Page,
  PreviewResult,
  Predicate,
  Result,
} from "./types.js";

function unimplemented(op: OperationDescriptor): { ok: false; error: Diagnostic } {
  return {
    ok: false,
    error: {
      status: "error",
      category: "TRANSPORT_FAILED",
      operation: op.id,
      http_status: null,
      code: null,
      failed_parameter: null,
      received_value: null,
      expected: null,
      remediation: "The tungsten runtime is not implemented yet.",
      retryable: "never",
      retry_after_ms: null,
      next_action: null,
      request_id: null,
      trace: { attempts: 0 },
    },
  };
}

/** The runtime core used by generated clients. */
export class ClientCore implements ClientCoreApi {
  readonly api: ApiDescriptor;
  readonly options: ClientOptions;

  constructor(api: ApiDescriptor, options: ClientOptions = {}) {
    this.api = api;
    this.options = options;
  }

  async call<T>(op: OperationDescriptor, _args: Record<string, unknown>, _opts?: CallOptions): Promise<Result<T>> {
    return unimplemented(op);
  }

  async preview(op: OperationDescriptor, _args: Record<string, unknown>, _opts?: CallOptions): Promise<Result<PreviewResult>> {
    return unimplemented(op);
  }

  async *pages<T>(op: OperationDescriptor, _args: Record<string, unknown>, _opts?: CallOptions): AsyncIterable<Result<Page<T>>> {
    yield unimplemented(op);
  }

  async poll<T>(
    op: OperationDescriptor,
    _args: Record<string, unknown>,
    _until: Predicate,
    _intervalMs: number,
    _budgetMs: number,
    _opts?: CallOptions,
  ): Promise<Result<T> & { timedOut?: boolean }> {
    return unimplemented(op);
  }
}
