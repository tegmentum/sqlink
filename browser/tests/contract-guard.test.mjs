// Node-level unit tests for the browser-side sqlite:extension contract
// guard (PLAN-wit-value-extension.md Phase F — F4). Pure-JS module; the
// node:test runner is enough — no playwright, no browser server.
//
// Run via:  node --test browser/tests/contract-guard.test.mjs

import { test } from 'node:test'
import { strict as assert } from 'node:assert'

import {
  CONTRACT_MAJOR,
  CONTRACT_PACKAGE,
  contractVersionString,
  componentContractMajor,
  checkComponentContract,
} from '../src/contract-guard.js'

/**
 * Build a tiny byte buffer that mimics a component-model binary
 * containing the given import-name string. componentContractMajor's
 * detection is regex-over-bytes, so the actual wasm header / structure
 * doesn't matter for the guard's purposes — only the literal substring
 * appearance does (which matches how the component model encodes its
 * length-prefixed UTF-8 import names in the binary).
 */
function fakeComponentWithImport(importName) {
  // Real component-model magic bytes so anyone inspecting the test
  // fixture sees the right header. Not strictly required by the guard
  // but it keeps the fixture self-explanatory.
  const header = new Uint8Array([
    0x00, 0x61, 0x73, 0x6d, // \0asm
    0x0d, 0x00, 0x01, 0x00, // component-model version + layer
  ])
  const importBytes = new TextEncoder().encode(importName)
  const out = new Uint8Array(header.length + importBytes.length + 16)
  out.set(header, 0)
  out.set(importBytes, header.length + 8)
  return out
}

test('CONTRACT_MAJOR is 1 (matches host/src/lib.rs constant)', () => {
  assert.equal(CONTRACT_MAJOR, 1)
  assert.equal(CONTRACT_PACKAGE, 'sqlite:extension')
  assert.equal(contractVersionString(), 'sqlite:extension@1.x')
})

test('componentContractMajor extracts major from versioned import', () => {
  const bytes = fakeComponentWithImport('sqlite:extension/types@1.0.0')
  assert.equal(componentContractMajor(bytes), 1)
})

test('componentContractMajor handles future-major bumps', () => {
  const bytes = fakeComponentWithImport('sqlite:extension/types@2.0.0')
  assert.equal(componentContractMajor(bytes), 2)
})

test('componentContractMajor handles legacy @0.1.0', () => {
  const bytes = fakeComponentWithImport('sqlite:extension/types@0.1.0')
  assert.equal(componentContractMajor(bytes), 0)
})

test('componentContractMajor returns null when no sqlite:extension import', () => {
  const bytes = fakeComponentWithImport('wasi:io/streams@0.2.6')
  assert.equal(componentContractMajor(bytes), null)
})

test('checkComponentContract passes when majors match', () => {
  const bytes = fakeComponentWithImport('sqlite:extension/types@1.0.0')
  assert.doesNotThrow(() => checkComponentContract(bytes, 'matching_ext'))
})

test('checkComponentContract rejects legacy @0.x with friendly message', () => {
  const bytes = fakeComponentWithImport('sqlite:extension/types@0.1.0')
  assert.throws(
    () => checkComponentContract(bytes, 'legacy_ext'),
    (err) => {
      const msg = err.message
      assert.match(msg, /legacy_ext/, `names extension: ${msg}`)
      assert.match(msg, /sqlite:extension contract 0\.x/, `targets 0.x: ${msg}`)
      assert.match(msg, /contract 1\.x/, `host 1.x: ${msg}`)
      assert.match(msg, /rebuild/, `actionable: ${msg}`)
      return true
    },
  )
})

test('checkComponentContract rejects future @2.x with friendly message', () => {
  const bytes = fakeComponentWithImport('sqlite:extension/types@2.0.0')
  assert.throws(
    () => checkComponentContract(bytes, 'future_ext'),
    (err) => {
      const msg = err.message
      assert.match(msg, /future_ext/, `names extension: ${msg}`)
      assert.match(msg, /sqlite:extension contract 2\.x/, `targets 2.x: ${msg}`)
      assert.match(msg, /contract 1\.x/, `host 1.x: ${msg}`)
      return true
    },
  )
})

test('checkComponentContract rejects unversioned/legacy with friendly message', () => {
  const bytes = fakeComponentWithImport('wasi:io/streams@0.2.6')
  assert.throws(
    () => checkComponentContract(bytes, 'preversioning'),
    (err) => {
      const msg = err.message
      assert.match(msg, /preversioning/, `names extension: ${msg}`)
      assert.match(msg, /UNVERSIONED/, `flags legacy: ${msg}`)
      assert.match(msg, /sqlite:extension/, `names package: ${msg}`)
      return true
    },
  )
})

test('checkComponentContract accepts ArrayBuffer input', () => {
  const u8 = fakeComponentWithImport('sqlite:extension/types@1.0.0')
  // ArrayBuffer slice — same backing memory but as a different view shape.
  const ab = u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength)
  assert.doesNotThrow(() => checkComponentContract(ab, 'ab_input'))
})
