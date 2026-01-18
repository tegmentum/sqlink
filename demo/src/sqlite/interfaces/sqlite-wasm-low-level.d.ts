/** @module Interface sqlite:wasm/low-level@0.1.0 **/
export function open(filename: string, openFlags: OpenFlags): DbHandle;
export function close(db: DbHandle): ResultCode;
export function exec(db: DbHandle, sql: string): string;
export function prepare(db: DbHandle, sql: string): StmtHandle;
export function step(stmt: StmtHandle): ResultCode;
export function reset(stmt: StmtHandle): ResultCode;
export function finalize(stmt: StmtHandle): ResultCode;
export function bindNull(stmt: StmtHandle, index: number): ResultCode;
export function bindInt(stmt: StmtHandle, index: number, value: number): ResultCode;
export function bindInt64(stmt: StmtHandle, index: number, value: bigint): ResultCode;
export function bindDouble(stmt: StmtHandle, index: number, value: number): ResultCode;
export function bindText(stmt: StmtHandle, index: number, value: string): ResultCode;
export function bindBlob(stmt: StmtHandle, index: number, value: Uint8Array): ResultCode;
export function bindParameterCount(stmt: StmtHandle): number;
export function bindParameterIndex(stmt: StmtHandle, name: string): number;
export function clearBindings(stmt: StmtHandle): ResultCode;
export function columnCount(stmt: StmtHandle): number;
export function columnName(stmt: StmtHandle, index: number): string;
export function getColumnType(stmt: StmtHandle, index: number): ColumnType;
export function columnInt(stmt: StmtHandle, index: number): number;
export function columnInt64(stmt: StmtHandle, index: number): bigint;
export function columnDouble(stmt: StmtHandle, index: number): number;
export function columnText(stmt: StmtHandle, index: number): string;
export function columnBlob(stmt: StmtHandle, index: number): Uint8Array;
export function columnBytes(stmt: StmtHandle, index: number): number;
export function errmsg(db: DbHandle): string;
export function errcode(db: DbHandle): ResultCode;
export function extendedErrcode(db: DbHandle): number;
export function getAutocommit(db: DbHandle): boolean;
export function changes(db: DbHandle): number;
export function totalChanges(db: DbHandle): number;
export function lastInsertRowid(db: DbHandle): bigint;
export function libversion(): string;
export function libversionNumber(): number;
export function sourceid(): string;
export type DbHandle = bigint;
export type StmtHandle = bigint;
/**
 * # Variants
 * 
 * ## `"ok"`
 * 
 * ## `"error"`
 * 
 * ## `"internal"`
 * 
 * ## `"perm"`
 * 
 * ## `"abort"`
 * 
 * ## `"busy"`
 * 
 * ## `"locked"`
 * 
 * ## `"nomem"`
 * 
 * ## `"readonly"`
 * 
 * ## `"interrupt"`
 * 
 * ## `"ioerr"`
 * 
 * ## `"corrupt"`
 * 
 * ## `"notfound"`
 * 
 * ## `"full"`
 * 
 * ## `"cantopen"`
 * 
 * ## `"protocol"`
 * 
 * ## `"empty"`
 * 
 * ## `"schema"`
 * 
 * ## `"toobig"`
 * 
 * ## `"constraint"`
 * 
 * ## `"mismatch"`
 * 
 * ## `"misuse"`
 * 
 * ## `"nolfs"`
 * 
 * ## `"auth"`
 * 
 * ## `"format"`
 * 
 * ## `"range"`
 * 
 * ## `"notadb"`
 * 
 * ## `"notice"`
 * 
 * ## `"warning"`
 * 
 * ## `"row"`
 * 
 * ## `"done"`
 */
export type ResultCode = 'ok' | 'error' | 'internal' | 'perm' | 'abort' | 'busy' | 'locked' | 'nomem' | 'readonly' | 'interrupt' | 'ioerr' | 'corrupt' | 'notfound' | 'full' | 'cantopen' | 'protocol' | 'empty' | 'schema' | 'toobig' | 'constraint' | 'mismatch' | 'misuse' | 'nolfs' | 'auth' | 'format' | 'range' | 'notadb' | 'notice' | 'warning' | 'row' | 'done';
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
export type ColumnType = 'integer' | 'float' | 'text' | 'blob' | 'null';
export interface OpenFlags {
  readonly?: boolean,
  readwrite?: boolean,
  create?: boolean,
  memory?: boolean,
  uri?: boolean,
}
