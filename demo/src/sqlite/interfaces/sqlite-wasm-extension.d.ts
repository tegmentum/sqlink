/** @module Interface sqlite:wasm/extension@0.1.0 **/
export function registerScalarFunction(db: DbHandle, name: string, numArgs: number, funcFlags: FunctionFlags, functionId: bigint): FunctionHandle;
export function unregisterFunction(handle: FunctionHandle): void;
export function registerAggregateFunction(db: DbHandle, name: string, numArgs: number, funcFlags: FunctionFlags, functionId: bigint): FunctionHandle;
export function registerCollation(db: DbHandle, name: string, collationId: bigint): CollationHandle;
export function unregisterCollation(handle: CollationHandle): void;
export function setUpdateHook(db: DbHandle, hookId: bigint): HookHandle;
export function removeUpdateHook(handle: HookHandle): void;
export function setCommitHook(db: DbHandle, hookId: bigint): HookHandle;
export function removeCommitHook(handle: HookHandle): void;
export function setRollbackHook(db: DbHandle, hookId: bigint): HookHandle;
export function removeRollbackHook(handle: HookHandle): void;
export function setBusyTimeout(db: DbHandle, ms: number): void;
export function setAuthorizer(db: DbHandle, authId: bigint): HookHandle;
export function removeAuthorizer(handle: HookHandle): void;
/**
 * # Variants
 * 
 * ## `"integer"`
 * 
 * ## `"float"`
 * 
 * ## `"text"`
 * 
 * ## `"blob"`
 * 
 * ## `"null"`
 */
export type ValueType = 'integer' | 'float' | 'text' | 'blob' | 'null';
export interface SqlValue {
  valueType: ValueType,
  intValue?: bigint,
  floatValue?: number,
  textValue?: string,
  blobValue?: Uint8Array,
}
/**
 * # Variants
 * 
 * ## `"insert"`
 * 
 * ## `"update"`
 * 
 * ## `"delete"`
 */
export type UpdateType = 'insert' | 'update' | 'delete';
/**
 * # Variants
 * 
 * ## `"create-index"`
 * 
 * ## `"create-table"`
 * 
 * ## `"create-temp-index"`
 * 
 * ## `"create-temp-table"`
 * 
 * ## `"create-temp-trigger"`
 * 
 * ## `"create-temp-view"`
 * 
 * ## `"create-trigger"`
 * 
 * ## `"create-view"`
 * 
 * ## `"delete"`
 * 
 * ## `"drop-index"`
 * 
 * ## `"drop-table"`
 * 
 * ## `"drop-temp-index"`
 * 
 * ## `"drop-temp-table"`
 * 
 * ## `"drop-temp-trigger"`
 * 
 * ## `"drop-temp-view"`
 * 
 * ## `"drop-trigger"`
 * 
 * ## `"drop-view"`
 * 
 * ## `"insert"`
 * 
 * ## `"pragma"`
 * 
 * ## `"read"`
 * 
 * ## `"select"`
 * 
 * ## `"transaction"`
 * 
 * ## `"update"`
 * 
 * ## `"attach"`
 * 
 * ## `"detach"`
 * 
 * ## `"alter-table"`
 * 
 * ## `"reindex"`
 * 
 * ## `"analyze"`
 * 
 * ## `"create-vtable"`
 * 
 * ## `"drop-vtable"`
 * 
 * ## `"function"`
 * 
 * ## `"savepoint"`
 * 
 * ## `"recursive"`
 */
export type AuthAction = 'create-index' | 'create-table' | 'create-temp-index' | 'create-temp-table' | 'create-temp-trigger' | 'create-temp-view' | 'create-trigger' | 'create-view' | 'delete' | 'drop-index' | 'drop-table' | 'drop-temp-index' | 'drop-temp-table' | 'drop-temp-trigger' | 'drop-temp-view' | 'drop-trigger' | 'drop-view' | 'insert' | 'pragma' | 'read' | 'select' | 'transaction' | 'update' | 'attach' | 'detach' | 'alter-table' | 'reindex' | 'analyze' | 'create-vtable' | 'drop-vtable' | 'function' | 'savepoint' | 'recursive';
/**
 * # Variants
 * 
 * ## `"ok"`
 * 
 * ## `"deny"`
 * 
 * ## `"ignore"`
 */
export type AuthResult = 'ok' | 'deny' | 'ignore';
export type FunctionHandle = bigint;
export type CollationHandle = bigint;
export type HookHandle = bigint;
export type DbHandle = bigint;
export interface FunctionFlags {
  deterministic?: boolean,
  directOnly?: boolean,
}
export interface ExtensionError {
  code: number,
  message: string,
}
