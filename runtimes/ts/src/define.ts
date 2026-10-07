// SPDX-License-Identifier: Apache-2.0
/**
 * Identity helpers for generated code: they return their argument
 * unchanged and exist so descriptor literals are checked against the
 * contract types while keeping their literal types (`satisfies` without
 * widening).
 */
import type { ApiDescriptor, MacroDescriptor, OperationDescriptor } from "./types.js";

/** Declare one operation descriptor. */
export function defineOperation<const T extends OperationDescriptor>(op: T): T {
  return op;
}

/** Declare the API descriptor of a client. */
export function defineApi<const T extends ApiDescriptor>(api: T): T {
  return api;
}

/** Declare a compiled macro. */
export function defineMacro<const T extends MacroDescriptor>(macro: T): T {
  return macro;
}
