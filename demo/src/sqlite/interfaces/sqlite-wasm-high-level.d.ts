/** @module Interface sqlite:wasm/high-level@0.1.0 **/
export function version(): string;
export function versionNumber(): number;
export function openMemory(): Connection;
export function openFile(path: string): Connection;
export type Value = ValueNull | ValueInteger | ValueReal | ValueText | ValueBlob;
export interface ValueNull {
  tag: 'null',
}
export interface ValueInteger {
  tag: 'integer',
  val: bigint,
}
export interface ValueReal {
  tag: 'real',
  val: number,
}
export interface ValueText {
  tag: 'text',
  val: string,
}
export interface ValueBlob {
  tag: 'blob',
  val: Uint8Array,
}
export interface DatabaseError {
  code: number,
  extendedCode: number,
  message: string,
}
export interface Row {
  columns: Array<Value>,
}
export interface QueryResult {
  columnNames: Array<string>,
  rows: Array<Row>,
}
export interface ExecResult {
  changes: number,
  lastInsertRowid: bigint,
}
/**
 * # Variants
 * 
 * ## `"read-only"`
 * 
 * ## `"read-write"`
 * 
 * ## `"read-write-create"`
 * 
 * ## `"memory"`
 */
export type OpenMode = 'read-only' | 'read-write' | 'read-write-create' | 'memory';

export class Connection {
  constructor(path: string, mode: OpenMode)
  execute(sql: string): ExecResult;
  executeWithParams(sql: string, params: Array<Value>): ExecResult;
  query(sql: string): QueryResult;
  queryWithParams(sql: string, params: Array<Value>): QueryResult;
  prepare(sql: string): Statement;
  beginTransaction(): void;
  commit(): void;
  rollback(): void;
  inAutocommit(): boolean;
  lastError(): DatabaseError | undefined;
}

export class Statement {
  /**
   * This type does not have a public constructor.
   */
  private constructor();
  bind(index: number, value: Value): void;
  bindAll(params: Array<Value>): void;
  execute(): ExecResult;
  query(): QueryResult;
  step(): Row | undefined;
  reset(): void;
  clearBindings(): void;
  columnCount(): number;
  columnNames(): Array<string>;
  parameterCount(): number;
}
