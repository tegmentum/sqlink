/** @module Interface sqlite:wasm/extension-callbacks@0.1.0 **/
export function onScalarFunction(functionId: bigint, args: Array<SqlValue>): SqlValue;
export function onAggregateStep(functionId: bigint, contextId: bigint, args: Array<SqlValue>): void;
export function onAggregateFinalize(functionId: bigint, contextId: bigint): SqlValue;
export function onCollationCompare(collationId: bigint, a: string, b: string): number;
export function onUpdate(hookId: bigint, op: UpdateType, database: string, table: string, rowid: bigint): void;
export function onCommit(hookId: bigint): boolean;
export function onRollback(hookId: bigint): void;
export function onAuthorize(authId: bigint, action: AuthAction, arg1: string | undefined, arg2: string | undefined, database: string | undefined, trigger: string | undefined): AuthResult;
export type SqlValue = import('./sqlite-wasm-extension.js').SqlValue;
export type UpdateType = import('./sqlite-wasm-extension.js').UpdateType;
export type AuthAction = import('./sqlite-wasm-extension.js').AuthAction;
export type AuthResult = import('./sqlite-wasm-extension.js').AuthResult;
