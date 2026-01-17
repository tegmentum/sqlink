export function instantiate(getCoreModule, imports, instantiateCore = WebAssembly.instantiate) {
  
  const i64ToF64I = new BigInt64Array(1);
  const i64ToF64F = new Float64Array(i64ToF64I.buffer);
  
  const emptyFunc = () => {};
  
  let dv = new DataView(new ArrayBuffer());
  const dataView = mem => dv.buffer === mem.buffer ? dv : dv = new DataView(mem.buffer);
  
  const f64ToI64 = f => (i64ToF64F[0] = f, i64ToF64I[0]);
  
  const toInt64 = val => BigInt.asIntN(64, BigInt(val));
  
  const toUint64 = val => BigInt.asUintN(64, BigInt(val));
  
  function toInt32(val) {
    return val >> 0;
  }
  
  function toUint32(val) {
    return val >>> 0;
  }
  
  const utf8Decoder = new TextDecoder();
  
  const utf8Encoder = new TextEncoder();
  let utf8EncodedLen = 0;
  function utf8Encode(s, realloc, memory) {
    if (typeof s !== 'string') throw new TypeError('expected a string');
    if (s.length === 0) {
      utf8EncodedLen = 0;
      return 1;
    }
    let buf = utf8Encoder.encode(s);
    let ptr = realloc(0, 0, 1, buf.length);
    new Uint8Array(memory.buffer).set(buf, ptr);
    utf8EncodedLen = buf.length;
    return ptr;
  }
  
  const T_FLAG = 1 << 30;
  
  function rscTableCreateOwn (table, rep) {
    const free = table[0] & ~T_FLAG;
    if (free === 0) {
      table.push(0);
      table.push(rep | T_FLAG);
      return (table.length >> 1) - 1;
    }
    table[0] = table[free << 1];
    table[free << 1] = 0;
    table[(free << 1) + 1] = rep | T_FLAG;
    return free;
  }
  
  function rscTableRemove (table, handle) {
    const scope = table[handle << 1];
    const val = table[(handle << 1) + 1];
    const own = (val & T_FLAG) !== 0;
    const rep = val & ~T_FLAG;
    if (val === 0 || (scope & T_FLAG) !== 0) throw new TypeError('Invalid handle');
    table[handle << 1] = table[0] | T_FLAG;
    table[0] = handle | T_FLAG;
    return { rep, scope, own };
  }
  
  let curResourceBorrows = [];
  
  let NEXT_TASK_ID = 0n;
  function startCurrentTask(componentIdx, isAsync, entryFnName) {
    _debugLog('[startCurrentTask()] args', { componentIdx, isAsync });
    if (componentIdx === undefined || componentIdx === null) {
      throw new Error('missing/invalid component instance index while starting task');
    }
    const tasks = ASYNC_TASKS_BY_COMPONENT_IDX.get(componentIdx);
    
    const nextId = ++NEXT_TASK_ID;
    const newTask = new AsyncTask({ id: nextId, componentIdx, isAsync, entryFnName });
    const newTaskMeta = { id: nextId, componentIdx, task: newTask };
    
    ASYNC_CURRENT_TASK_IDS.push(nextId);
    ASYNC_CURRENT_COMPONENT_IDXS.push(componentIdx);
    
    if (!tasks) {
      ASYNC_TASKS_BY_COMPONENT_IDX.set(componentIdx, [newTaskMeta]);
      return nextId;
    } else {
      tasks.push(newTaskMeta);
    }
    
    return nextId;
  }
  
  function endCurrentTask(componentIdx, taskId) {
    _debugLog('[endCurrentTask()] args', { componentIdx });
    componentIdx ??= ASYNC_CURRENT_COMPONENT_IDXS.at(-1);
    taskId ??= ASYNC_CURRENT_TASK_IDS.at(-1);
    if (componentIdx === undefined || componentIdx === null) {
      throw new Error('missing/invalid component instance index while ending current task');
    }
    const tasks = ASYNC_TASKS_BY_COMPONENT_IDX.get(componentIdx);
    if (!tasks || !Array.isArray(tasks)) {
      throw new Error('missing/invalid tasks for component instance while ending task');
    }
    if (tasks.length == 0) {
      throw new Error('no current task(s) for component instance while ending task');
    }
    
    if (taskId) {
      const last = tasks[tasks.length - 1];
      if (last.id !== taskId) {
        throw new Error('current task does not match expected task ID');
      }
    }
    
    ASYNC_CURRENT_TASK_IDS.pop();
    ASYNC_CURRENT_COMPONENT_IDXS.pop();
    
    return tasks.pop();
  }
  const ASYNC_TASKS_BY_COMPONENT_IDX = new Map();
  const ASYNC_CURRENT_TASK_IDS = [];
  const ASYNC_CURRENT_COMPONENT_IDXS = [];
  
  class AsyncTask {
    static State = {
      INITIAL: 'initial',
      CANCELLED: 'cancelled',
      CANCEL_PENDING: 'cancel-pending',
      CANCEL_DELIVERED: 'cancel-delivered',
      RESOLVED: 'resolved',
    }
    
    static BlockResult = {
      CANCELLED: 'block.cancelled',
      NOT_CANCELLED: 'block.not-cancelled',
    }
    
    #id;
    #componentIdx;
    #state;
    #isAsync;
    #onResolve = null;
    #entryFnName = null;
    #subtasks = [];
    #completionPromise = null;
    
    cancelled = false;
    requested = false;
    alwaysTaskReturn = false;
    
    returnCalls =  0;
    storage = [0, 0];
    borrowedHandles = {};
    
    awaitableResume = null;
    awaitableCancel = null;
    
    
    constructor(opts) {
      if (opts?.id === undefined) { throw new TypeError('missing task ID during task creation'); }
      this.#id = opts.id;
      if (opts?.componentIdx === undefined) {
        throw new TypeError('missing component id during task creation');
      }
      this.#componentIdx = opts.componentIdx;
      this.#state = AsyncTask.State.INITIAL;
      this.#isAsync = opts?.isAsync ?? false;
      this.#entryFnName = opts.entryFnName;
      
      const {
        promise: completionPromise,
        resolve: resolveCompletionPromise,
        reject: rejectCompletionPromise,
      } = Promise.withResolvers();
      this.#completionPromise = completionPromise;
      
      this.#onResolve = (results) => {
        // TODO: handle external facing cancellation (should likely be a rejection)
        resolveCompletionPromise(results);
      }
    }
    
    taskState() { return this.#state.slice(); }
    id() { return this.#id; }
    componentIdx() { return this.#componentIdx; }
    isAsync() { return this.#isAsync; }
    entryFnName() { return this.#entryFnName; }
    completionPromise() { return this.#completionPromise; }
    
    mayEnter(task) {
      const cstate = getOrCreateAsyncState(this.#componentIdx);
      if (!cstate.backpressure) {
        _debugLog('[AsyncTask#mayEnter()] disallowed due to backpressure', { taskID: this.#id });
        return false;
      }
      if (!cstate.callingSyncImport()) {
        _debugLog('[AsyncTask#mayEnter()] disallowed due to sync import call', { taskID: this.#id });
        return false;
      }
      const callingSyncExportWithSyncPending = cstate.callingSyncExport && !task.isAsync;
      if (!callingSyncExportWithSyncPending) {
        _debugLog('[AsyncTask#mayEnter()] disallowed due to sync export w/ sync pending', { taskID: this.#id });
        return false;
      }
      return true;
    }
    
    async enter() {
      _debugLog('[AsyncTask#enter()] args', { taskID: this.#id });
      
      // TODO: assert scheduler locked
      // TODO: trap if on the stack
      
      const cstate = getOrCreateAsyncState(this.#componentIdx);
      
      let mayNotEnter = !this.mayEnter(this);
      const componentHasPendingTasks = cstate.pendingTasks > 0;
      if (mayNotEnter || componentHasPendingTasks) {
        throw new Error('in enter()'); // TODO: remove
        cstate.pendingTasks.set(this.#id, new Awaitable(new Promise()));
        
        const blockResult = await this.onBlock(awaitable);
        if (blockResult) {
          // TODO: find this pending task in the component
          const pendingTask = cstate.pendingTasks.get(this.#id);
          if (!pendingTask) {
            throw new Error('pending task [' + this.#id + '] not found for component instance');
          }
          cstate.pendingTasks.remove(this.#id);
          this.#onResolve(new Error('failed enter'));
          return false;
        }
        
        mayNotEnter = !this.mayEnter(this);
        if (!mayNotEnter || !cstate.startPendingTask) {
          throw new Error('invalid component entrance/pending task resolution');
        }
        cstate.startPendingTask = false;
      }
      
      if (!this.isAsync) { cstate.callingSyncExport = true; }
      
      return true;
    }
    
    async waitForEvent(opts) {
      const { waitableSetRep, isAsync } = opts;
      _debugLog('[AsyncTask#waitForEvent()] args', { taskID: this.#id, waitableSetRep, isAsync });
      
      if (this.#isAsync !== isAsync) {
        throw new Error('async waitForEvent called on non-async task');
      }
      
      if (this.status === AsyncTask.State.CANCEL_PENDING) {
        this.#state = AsyncTask.State.CANCEL_DELIVERED;
        return {
          code: ASYNC_EVENT_CODE.TASK_CANCELLED,
        };
      }
      
      const state = getOrCreateAsyncState(this.#componentIdx);
      const waitableSet = state.waitableSets.get(waitableSetRep);
      if (!waitableSet) { throw new Error('missing/invalid waitable set'); }
      
      waitableSet.numWaiting += 1;
      let event = null;
      
      while (event == null) {
        const awaitable = new Awaitable(waitableSet.getPendingEvent());
        const waited = await this.blockOn({ awaitable, isAsync, isCancellable: true });
        if (waited) {
          if (this.#state !== AsyncTask.State.INITIAL) {
            throw new Error('task should be in initial state found [' + this.#state + ']');
          }
          this.#state = AsyncTask.State.CANCELLED;
          return {
            code: ASYNC_EVENT_CODE.TASK_CANCELLED,
          };
        }
        
        event = waitableSet.poll();
      }
      
      waitableSet.numWaiting -= 1;
      return event;
    }
    
    waitForEventSync(opts) {
      throw new Error('AsyncTask#yieldSync() not implemented')
    }
    
    async pollForEvent(opts) {
      const { waitableSetRep, isAsync } = opts;
      _debugLog('[AsyncTask#pollForEvent()] args', { taskID: this.#id, waitableSetRep, isAsync });
      
      if (this.#isAsync !== isAsync) {
        throw new Error('async pollForEvent called on non-async task');
      }
      
      throw new Error('AsyncTask#pollForEvent() not implemented');
    }
    
    pollForEventSync(opts) {
      throw new Error('AsyncTask#yieldSync() not implemented')
    }
    
    async blockOn(opts) {
      const { awaitable, isCancellable, forCallback } = opts;
      _debugLog('[AsyncTask#blockOn()] args', { taskID: this.#id, awaitable, isCancellable, forCallback });
      
      if (awaitable.resolved() && !ASYNC_DETERMINISM && _coinFlip()) {
        return AsyncTask.BlockResult.NOT_CANCELLED;
      }
      
      const cstate = getOrCreateAsyncState(this.#componentIdx);
      if (forCallback) { cstate.exclusiveRelease(); }
      
      let cancelled = await this.onBlock(awaitable);
      if (cancelled === AsyncTask.BlockResult.CANCELLED && !isCancellable) {
        const secondCancel = await this.onBlock(awaitable);
        if (secondCancel !== AsyncTask.BlockResult.NOT_CANCELLED) {
          throw new Error('uncancellable task was canceled despite second onBlock()');
        }
      }
      
      if (forCallback) {
        const acquired = new Awaitable(cstate.exclusiveLock());
        cancelled = await this.onBlock(acquired);
        if (cancelled === AsyncTask.BlockResult.CANCELLED) {
          const secondCancel = await this.onBlock(acquired);
          if (secondCancel !== AsyncTask.BlockResult.NOT_CANCELLED) {
            throw new Error('uncancellable callback task was canceled despite second onBlock()');
          }
        }
      }
      
      if (cancelled === AsyncTask.BlockResult.CANCELLED) {
        if (this.#state !== AsyncTask.State.INITIAL) {
          throw new Error('cancelled task is not at initial state');
        }
        if (isCancellable) {
          this.#state = AsyncTask.State.CANCELLED;
          return AsyncTask.BlockResult.CANCELLED;
        } else {
          this.#state = AsyncTask.State.CANCEL_PENDING;
          return AsyncTask.BlockResult.NOT_CANCELLED;
        }
      }
      
      return AsyncTask.BlockResult.NOT_CANCELLED;
    }
    
    async onBlock(awaitable) {
      _debugLog('[AsyncTask#onBlock()] args', { taskID: this.#id, awaitable });
      if (!(awaitable instanceof Awaitable)) {
        throw new Error('invalid awaitable during onBlock');
      }
      
      // Build a promise that this task can await on which resolves when it is awoken
      const { promise, resolve, reject } = Promise.withResolvers();
      this.awaitableResume = () => {
        _debugLog('[AsyncTask] resuming after onBlock', { taskID: this.#id });
        resolve();
      };
      this.awaitableCancel = (err) => {
        _debugLog('[AsyncTask] rejecting after onBlock', { taskID: this.#id, err });
        reject(err);
      };
      
      // Park this task/execution to be handled later
      const state = getOrCreateAsyncState(this.#componentIdx);
      state.parkTaskOnAwaitable({ awaitable, task: this });
      
      try {
        await promise;
        return AsyncTask.BlockResult.NOT_CANCELLED;
      } catch (err) {
        // rejection means task cancellation
        return AsyncTask.BlockResult.CANCELLED;
      }
    }
    
    async asyncOnBlock(awaitable) {
      _debugLog('[AsyncTask#asyncOnBlock()] args', { taskID: this.#id, awaitable });
      if (!(awaitable instanceof Awaitable)) {
        throw new Error('invalid awaitable during onBlock');
      }
      // TODO: watch for waitable AND cancellation
      // TODO: if it WAS cancelled:
      // - return true
      // - only once per subtask
      // - do not wait on the scheduler
      // - control flow should go to the subtask (only once)
      // - Once subtask blocks/resolves, reqlinquishControl() will tehn resolve request_cancel_end (without scheduler lock release)
      // - control flow goes back to request_cancel
      //
      // Subtask cancellation should work similarly to an async import call -- runs sync up until
      // the subtask blocks or resolves
      //
      throw new Error('AsyncTask#asyncOnBlock() not yet implemented');
    }
    
    async yield(opts) {
      const { isCancellable, forCallback } = opts;
      _debugLog('[AsyncTask#yield()] args', { taskID: this.#id, isCancellable, forCallback });
      
      if (isCancellable && this.status === AsyncTask.State.CANCEL_PENDING) {
        this.#state = AsyncTask.State.CANCELLED;
        return {
          code: ASYNC_EVENT_CODE.TASK_CANCELLED,
          payload: [0, 0],
        };
      }
      
      // TODO: Awaitables need to *always* trigger the parking mechanism when they're done...?
      // TODO: Component async state should remember which awaitables are done and work to clear tasks waiting
      
      const blockResult = await this.blockOn({
        awaitable: new Awaitable(new Promise(resolve => setTimeout(resolve, 0))),
        isCancellable,
        forCallback,
      });
      
      if (blockResult === AsyncTask.BlockResult.CANCELLED) {
        if (this.#state !== AsyncTask.State.INITIAL) {
          throw new Error('task should be in initial state found [' + this.#state + ']');
        }
        this.#state = AsyncTask.State.CANCELLED;
        return {
          code: ASYNC_EVENT_CODE.TASK_CANCELLED,
          payload: [0, 0],
        };
      }
      
      return {
        code: ASYNC_EVENT_CODE.NONE,
        payload: [0, 0],
      };
    }
    
    yieldSync(opts) {
      throw new Error('AsyncTask#yieldSync() not implemented')
    }
    
    cancel() {
      _debugLog('[AsyncTask#cancel()] args', { });
      if (!this.taskState() !== AsyncTask.State.CANCEL_DELIVERED) {
        throw new Error('invalid task state for cancellation');
      }
      if (this.borrowedHandles.length > 0) { throw new Error('task still has borrow handles'); }
      
      this.#onResolve(new Error('cancelled'));
      this.#state = AsyncTask.State.RESOLVED;
    }
    
    resolve(results) {
      _debugLog('[AsyncTask#resolve()] args', { results });
      if (this.#state === AsyncTask.State.RESOLVED) {
        throw new Error('task is already resolved');
      }
      if (this.borrowedHandles.length > 0) { throw new Error('task still has borrow handles'); }
      this.#onResolve(results.length === 1 ? results[0] : results);
      this.#state = AsyncTask.State.RESOLVED;
    }
    
    exit() {
      _debugLog('[AsyncTask#exit()] args', { });
      
      // TODO: ensure there is only one task at a time (scheduler.lock() functionality)
      if (this.#state !== AsyncTask.State.RESOLVED) {
        throw new Error('task exited without resolution');
      }
      if (this.borrowedHandles > 0) {
        throw new Error('task exited without clearing borrowed handles');
      }
      
      const state = getOrCreateAsyncState(this.#componentIdx);
      if (!state) { throw new Error('missing async state for component [' + this.#componentIdx + ']'); }
      if (!this.#isAsync && !state.inSyncExportCall) {
        throw new Error('sync task must be run from components known to be in a sync export call');
      }
      state.inSyncExportCall = false;
      
      this.startPendingTask();
    }
    
    startPendingTask(args) {
      _debugLog('[AsyncTask#startPendingTask()] args', args);
      throw new Error('AsyncTask#startPendingTask() not implemented');
    }
    
    createSubtask(args) {
      _debugLog('[AsyncTask#createSubtask()] args', args);
      const newSubtask = new AsyncSubtask({
        componentIdx: this.componentIdx(),
        taskID: this.id(),
        memoryIdx: args?.memoryIdx,
      });
      this.#subtasks.push(newSubtask);
      return newSubtask;
    }
    
    currentSubtask() {
      _debugLog('[AsyncTask#currentSubtask()]');
      if (this.#subtasks.length === 0) { throw new Error('no current subtask'); }
      return this.#subtasks.at(-1);
    }
    
    endCurrentSubtask() {
      _debugLog('[AsyncTask#endCurrentSubtask()]');
      if (this.#subtasks.length === 0) { throw new Error('cannot end current subtask: no current subtask'); }
      const subtask = this.#subtasks.pop();
      subtask.drop();
      return subtask;
    }
  }
  
  function unpackCallbackResult(result) {
    _debugLog('[unpackCallbackResult()] args', { result });
    if (!(_typeCheckValidI32(result))) { throw new Error('invalid callback return value [' + result + '], not a valid i32'); }
    const eventCode = result & 0xF;
    if (eventCode < 0 || eventCode > 3) {
      throw new Error('invalid async return value [' + eventCode + '], outside callback code range');
    }
    if (result < 0 || result >= 2**32) { throw new Error('invalid callback result'); }
    // TODO: table max length check?
    const waitableSetIdx = result >> 4;
    return [eventCode, waitableSetIdx];
  }
  const ASYNC_STATE = new Map();
  
  function getOrCreateAsyncState(componentIdx, init) {
    if (!ASYNC_STATE.has(componentIdx)) {
      ASYNC_STATE.set(componentIdx, new ComponentAsyncState());
    }
    return ASYNC_STATE.get(componentIdx);
  }
  
  class ComponentAsyncState {
    #callingAsyncImport = false;
    #syncImportWait = Promise.withResolvers();
    #lock = null;
    
    mayLeave = true;
    waitableSets = new RepTable();
    waitables = new RepTable();
    
    #parkedTasks = new Map();
    
    callingSyncImport(val) {
      if (val === undefined) { return this.#callingAsyncImport; }
      if (typeof val !== 'boolean') { throw new TypeError('invalid setting for async import'); }
      const prev = this.#callingAsyncImport;
      this.#callingAsyncImport = val;
      if (prev === true && this.#callingAsyncImport === false) {
        this.#notifySyncImportEnd();
      }
    }
    
    #notifySyncImportEnd() {
      const existing = this.#syncImportWait;
      this.#syncImportWait = Promise.withResolvers();
      existing.resolve();
    }
    
    async waitForSyncImportCallEnd() {
      await this.#syncImportWait.promise;
    }
    
    parkTaskOnAwaitable(args) {
      if (!args.awaitable) { throw new TypeError('missing awaitable when trying to park'); }
      if (!args.task) { throw new TypeError('missing task when trying to park'); }
      const { awaitable, task } = args;
      
      let taskList = this.#parkedTasks.get(awaitable.id());
      if (!taskList) {
        taskList = [];
        this.#parkedTasks.set(awaitable.id(), taskList);
      }
      taskList.push(task);
      
      this.wakeNextTaskForAwaitable(awaitable);
    }
    
    wakeNextTaskForAwaitable(awaitable) {
      if (!awaitable) { throw new TypeError('missing awaitable when waking next task'); }
      const awaitableID = awaitable.id();
      
      const taskList = this.#parkedTasks.get(awaitableID);
      if (!taskList || taskList.length === 0) {
        _debugLog('[ComponentAsyncState] no tasks waiting for awaitable', { awaitableID: awaitable.id() });
        return;
      }
      
      let task = taskList.shift(); // todo(perf)
      if (!task) { throw new Error('no task in parked list despite previous check'); }
      
      if (!task.awaitableResume) {
        throw new Error('task ready due to awaitable is missing resume', { taskID: task.id(), awaitableID });
      }
      task.awaitableResume();
    }
    
    async exclusiveLock() {  // TODO: use atomics
    if (this.#lock === null) {
      this.#lock = { ticket: 0n };
    }
    
    // Take a ticket for the next valid usage
    const ticket = ++this.#lock.ticket;
    
    _debugLog('[ComponentAsyncState#exclusiveLock()] locking', {
      currentTicket: ticket - 1n,
      ticket
    });
    
    // If there is an active promise, then wait for it
    let finishedTicket;
    while (this.#lock.promise) {
      finishedTicket = await this.#lock.promise;
      if (finishedTicket === ticket - 1n) { break; }
    }
    
    const { promise, resolve } = Promise.withResolvers();
    this.#lock = {
      ticket,
      promise,
      resolve,
    };
    
    return this.#lock.promise;
  }
  
  exclusiveRelease() {
    _debugLog('[ComponentAsyncState#exclusiveRelease()] releasing', {
      currentTicket: this.#lock === null ? 'none' : this.#lock.ticket,
    });
    
    if (this.#lock === null) { return; }
    
    const existingLock = this.#lock;
    this.#lock = null;
    existingLock.resolve(existingLock.ticket);
  }
  
  isExclusivelyLocked() { return this.#lock !== null; }
  
}

function prepareCall(memoryIdx) {
  _debugLog('[prepareCall()] args', { memoryIdx });
  
  const taskMeta = getCurrentTask(ASYNC_CURRENT_COMPONENT_IDXS.at(-1), ASYNC_CURRENT_TASK_IDS.at(-1));
  if (!taskMeta) { throw new Error('invalid/missing current async task meta during prepare call'); }
  
  const task = taskMeta.task;
  if (!task) { throw new Error('unexpectedly missing task in task meta during prepare call'); }
  
  const state = getOrCreateAsyncState(task.componentIdx());
  if (!state) {
    throw new Error('invalid/missing async state for component instance [' + componentInstanceID + ']');
  }
  
  const subtask = task.createSubtask({
    memoryIdx,
  });
  
}

function asyncStartCall(callbackIdx, postReturnIdx) {
  _debugLog('[asyncStartCall()] args', { callbackIdx, postReturnIdx });
  
  const taskMeta = getCurrentTask(ASYNC_CURRENT_COMPONENT_IDXS.at(-1), ASYNC_CURRENT_TASK_IDS.at(-1));
  if (!taskMeta) { throw new Error('invalid/missing current async task meta during prepare call'); }
  
  const task = taskMeta.task;
  if (!task) { throw new Error('unexpectedly missing task in task meta during prepare call'); }
  
  const subtask = task.currentSubtask();
  if (!subtask) { throw new Error('invalid/missing subtask during async start call'); }
  
  return Number(subtask.waitableRep()) << 4 | subtask.getStateNumber();
}

function syncStartCall(callbackIdx) {
  _debugLog('[syncStartCall()] args', { callbackIdx });
}

if (!Promise.withResolvers) {
  Promise.withResolvers = () => {
    let resolve;
    let reject;
    const promise = new Promise((res, rej) => {
      resolve = res;
      reject = rej;
    });
    return { promise, resolve, reject };
  };
}

const _debugLog = (...args) => {
  if (!globalThis?.process?.env?.JCO_DEBUG) { return; }
  console.debug(...args);
}
const ASYNC_DETERMINISM = 'random';
const _coinFlip = () => { return Math.random() > 0.5; };
const I32_MAX = 2_147_483_647;
const I32_MIN = -2_147_483_648;
const _typeCheckValidI32 = (n) => typeof n === 'number' && n >= I32_MIN && n <= I32_MAX;

const isNode = typeof process !== 'undefined' && process.versions && process.versions.node;
let _fs;
async function fetchCompile (url) {
  if (isNode) {
    _fs = _fs || await import('node:fs/promises');
    return WebAssembly.compile(await _fs.readFile(url));
  }
  return fetch(url).then(WebAssembly.compileStreaming);
}

const symbolCabiDispose = Symbol.for('cabiDispose');

const symbolRscHandle = Symbol('handle');

const symbolRscRep = Symbol.for('cabiRep');

const symbolDispose = Symbol.dispose || Symbol.for('dispose');

const handleTables = [];

function finalizationRegistryCreate (unregister) {
  if (typeof FinalizationRegistry === 'undefined') {
    return { unregister () {} };
  }
  return new FinalizationRegistry(unregister);
}

class ComponentError extends Error {
  constructor (value) {
    const enumerable = typeof value !== 'string';
    super(enumerable ? `${String(value)} (see error.payload)` : value);
    Object.defineProperty(this, 'payload', { value, enumerable });
  }
}

function getErrorPayload(e) {
  if (e && hasOwnProperty.call(e, 'payload')) return e.payload;
  if (e instanceof Error) throw e;
  return e;
}

class RepTable {
  #data = [0, null];
  
  insert(val) {
    _debugLog('[RepTable#insert()] args', { val });
    const freeIdx = this.#data[0];
    if (freeIdx === 0) {
      this.#data.push(val);
      this.#data.push(null);
      return (this.#data.length >> 1) - 1;
    }
    this.#data[0] = this.#data[freeIdx << 1];
    const placementIdx = freeIdx << 1;
    this.#data[placementIdx] = val;
    this.#data[placementIdx + 1] = null;
    return freeIdx;
  }
  
  get(rep) {
    _debugLog('[RepTable#get()] args', { rep });
    const baseIdx = rep << 1;
    const val = this.#data[baseIdx];
    return val;
  }
  
  contains(rep) {
    _debugLog('[RepTable#contains()] args', { rep });
    const baseIdx = rep << 1;
    return !!this.#data[baseIdx];
  }
  
  remove(rep) {
    _debugLog('[RepTable#remove()] args', { rep });
    if (this.#data.length === 2) { throw new Error('invalid'); }
    
    const baseIdx = rep << 1;
    const val = this.#data[baseIdx];
    if (val === 0) { throw new Error('invalid resource rep (cannot be 0)'); }
    
    this.#data[baseIdx] = this.#data[0];
    this.#data[0] = rep;
    
    return val;
  }
  
  clear() {
    _debugLog('[RepTable#clear()] args', { rep });
    this.#data = [0, null];
  }
}

function throwInvalidBool() {
  throw new TypeError('invalid variant discriminant for bool');
}

const hasOwnProperty = Object.prototype.hasOwnProperty;


if (!getCoreModule) getCoreModule = (name) => fetchCompile(new URL(`./${name}`, import.meta.url));
const module0 = getCoreModule('sqlite.core.wasm');
const module1 = getCoreModule('sqlite.core2.wasm');
const module2 = getCoreModule('sqlite.core3.wasm');
const module3 = getCoreModule('sqlite.core4.wasm');
const module4 = getCoreModule('sqlite.core5.wasm');

const { exit } = imports['wasi:cli/exit'];
const { getStderr } = imports['wasi:cli/stderr'];
const { getStdin } = imports['wasi:cli/stdin'];
const { getStdout } = imports['wasi:cli/stdout'];
const { TerminalInput } = imports['wasi:cli/terminal-input'];
const { TerminalOutput } = imports['wasi:cli/terminal-output'];
const { getTerminalStderr } = imports['wasi:cli/terminal-stderr'];
const { getTerminalStdin } = imports['wasi:cli/terminal-stdin'];
const { getTerminalStdout } = imports['wasi:cli/terminal-stdout'];
const { now } = imports['wasi:clocks/monotonic-clock'];
const { now: now$1 } = imports['wasi:clocks/wall-clock'];
const { getDirectories } = imports['wasi:filesystem/preopens'];
const { Descriptor, filesystemErrorCode } = imports['wasi:filesystem/types'];
const { Error: Error$1 } = imports['wasi:io/error'];
const { InputStream, OutputStream } = imports['wasi:io/streams'];
let gen = (function* _initGenerator () {
  let exports0;
  let exports1;
  
  function trampoline9() {
    _debugLog('[iface="wasi:clocks/monotonic-clock@0.2.0", function="now"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'now');
    const ret = now();
    _debugLog('[iface="wasi:clocks/monotonic-clock@0.2.0", function="now"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    _debugLog('[iface="wasi:clocks/monotonic-clock@0.2.0", function="now"][Instruction::Return]', {
      funcName: 'now',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return toUint64(ret);
  }
  
  const handleTable3 = [T_FLAG, 0];
  const captureTable3= new Map();
  let captureCnt3 = 0;
  handleTables[3] = handleTable3;
  
  function trampoline13() {
    _debugLog('[iface="wasi:cli/stderr@0.2.0", function="get-stderr"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-stderr');
    const ret = getStderr();
    _debugLog('[iface="wasi:cli/stderr@0.2.0", function="get-stderr"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    if (!(ret instanceof OutputStream)) {
      throw new TypeError('Resource error: Not a valid "OutputStream" resource.');
    }
    var handle0 = ret[symbolRscHandle];
    if (!handle0) {
      const rep = ret[symbolRscRep] || ++captureCnt3;
      captureTable3.set(rep, ret);
      handle0 = rscTableCreateOwn(handleTable3, rep);
    }
    _debugLog('[iface="wasi:cli/stderr@0.2.0", function="get-stderr"][Instruction::Return]', {
      funcName: 'get-stderr',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return handle0;
  }
  
  const handleTable2 = [T_FLAG, 0];
  const captureTable2= new Map();
  let captureCnt2 = 0;
  handleTables[2] = handleTable2;
  
  function trampoline16() {
    _debugLog('[iface="wasi:cli/stdin@0.2.0", function="get-stdin"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-stdin');
    const ret = getStdin();
    _debugLog('[iface="wasi:cli/stdin@0.2.0", function="get-stdin"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    if (!(ret instanceof InputStream)) {
      throw new TypeError('Resource error: Not a valid "InputStream" resource.');
    }
    var handle0 = ret[symbolRscHandle];
    if (!handle0) {
      const rep = ret[symbolRscRep] || ++captureCnt2;
      captureTable2.set(rep, ret);
      handle0 = rscTableCreateOwn(handleTable2, rep);
    }
    _debugLog('[iface="wasi:cli/stdin@0.2.0", function="get-stdin"][Instruction::Return]', {
      funcName: 'get-stdin',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return handle0;
  }
  
  
  function trampoline17() {
    _debugLog('[iface="wasi:cli/stdout@0.2.0", function="get-stdout"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-stdout');
    const ret = getStdout();
    _debugLog('[iface="wasi:cli/stdout@0.2.0", function="get-stdout"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    if (!(ret instanceof OutputStream)) {
      throw new TypeError('Resource error: Not a valid "OutputStream" resource.');
    }
    var handle0 = ret[symbolRscHandle];
    if (!handle0) {
      const rep = ret[symbolRscRep] || ++captureCnt3;
      captureTable3.set(rep, ret);
      handle0 = rscTableCreateOwn(handleTable3, rep);
    }
    _debugLog('[iface="wasi:cli/stdout@0.2.0", function="get-stdout"][Instruction::Return]', {
      funcName: 'get-stdout',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return handle0;
  }
  
  
  function trampoline18(arg0) {
    let variant0;
    switch (arg0) {
      case 0: {
        variant0= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        variant0= {
          tag: 'err',
          val: undefined
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="wasi:cli/exit@0.2.0", function="exit"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'exit');
    exit(variant0);
    _debugLog('[iface="wasi:cli/exit@0.2.0", function="exit"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    _debugLog('[iface="wasi:cli/exit@0.2.0", function="exit"][Instruction::Return]', {
      funcName: 'exit',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  let exports2;
  let memory0;
  let realloc0;
  
  function trampoline19(arg0) {
    _debugLog('[iface="wasi:clocks/wall-clock@0.2.0", function="now"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'now');
    const ret = now$1();
    _debugLog('[iface="wasi:clocks/wall-clock@0.2.0", function="now"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    var {seconds: v0_0, nanoseconds: v0_1 } = ret;
    dataView(memory0).setBigInt64(arg0 + 0, toUint64(v0_0), true);
    dataView(memory0).setInt32(arg0 + 8, toUint32(v0_1), true);
    _debugLog('[iface="wasi:clocks/wall-clock@0.2.0", function="now"][Instruction::Return]', {
      funcName: 'now',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  const handleTable6 = [T_FLAG, 0];
  const captureTable6= new Map();
  let captureCnt6 = 0;
  handleTables[6] = handleTable6;
  
  function trampoline20(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.get-flags"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.get-flags');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.getFlags()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.get-flags"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        let flags3 = 0;
        if (typeof e === 'object' && e !== null) {
          flags3 = Boolean(e.read) << 0 | Boolean(e.write) << 1 | Boolean(e.fileIntegritySync) << 2 | Boolean(e.dataIntegritySync) << 3 | Boolean(e.requestedWriteSync) << 4 | Boolean(e.mutateDirectory) << 5;
        } else if (e !== null && e!== undefined) {
          throw new TypeError('only an object, undefined or null can be converted to flags');
        }
        dataView(memory0).setInt8(arg1 + 1, flags3, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg1 + 1, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.get-flags"][Instruction::Return]', {
      funcName: '[method]descriptor.get-flags',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline21(arg0, arg1, arg2) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.set-size"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.set-size');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.setSize(BigInt.asUintN(64, arg1))};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.set-size"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant4 = ret;
    switch (variant4.tag) {
      case 'ok': {
        const e = variant4.val;
        dataView(memory0).setInt8(arg2 + 0, 0, true);
        break;
      }
      case 'err': {
        const e = variant4.val;
        dataView(memory0).setInt8(arg2 + 0, 1, true);
        var val3 = e;
        let enum3;
        switch (val3) {
          case 'access': {
            enum3 = 0;
            break;
          }
          case 'would-block': {
            enum3 = 1;
            break;
          }
          case 'already': {
            enum3 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum3 = 3;
            break;
          }
          case 'busy': {
            enum3 = 4;
            break;
          }
          case 'deadlock': {
            enum3 = 5;
            break;
          }
          case 'quota': {
            enum3 = 6;
            break;
          }
          case 'exist': {
            enum3 = 7;
            break;
          }
          case 'file-too-large': {
            enum3 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum3 = 9;
            break;
          }
          case 'in-progress': {
            enum3 = 10;
            break;
          }
          case 'interrupted': {
            enum3 = 11;
            break;
          }
          case 'invalid': {
            enum3 = 12;
            break;
          }
          case 'io': {
            enum3 = 13;
            break;
          }
          case 'is-directory': {
            enum3 = 14;
            break;
          }
          case 'loop': {
            enum3 = 15;
            break;
          }
          case 'too-many-links': {
            enum3 = 16;
            break;
          }
          case 'message-size': {
            enum3 = 17;
            break;
          }
          case 'name-too-long': {
            enum3 = 18;
            break;
          }
          case 'no-device': {
            enum3 = 19;
            break;
          }
          case 'no-entry': {
            enum3 = 20;
            break;
          }
          case 'no-lock': {
            enum3 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum3 = 22;
            break;
          }
          case 'insufficient-space': {
            enum3 = 23;
            break;
          }
          case 'not-directory': {
            enum3 = 24;
            break;
          }
          case 'not-empty': {
            enum3 = 25;
            break;
          }
          case 'not-recoverable': {
            enum3 = 26;
            break;
          }
          case 'unsupported': {
            enum3 = 27;
            break;
          }
          case 'no-tty': {
            enum3 = 28;
            break;
          }
          case 'no-such-device': {
            enum3 = 29;
            break;
          }
          case 'overflow': {
            enum3 = 30;
            break;
          }
          case 'not-permitted': {
            enum3 = 31;
            break;
          }
          case 'pipe': {
            enum3 = 32;
            break;
          }
          case 'read-only': {
            enum3 = 33;
            break;
          }
          case 'invalid-seek': {
            enum3 = 34;
            break;
          }
          case 'text-file-busy': {
            enum3 = 35;
            break;
          }
          case 'cross-device': {
            enum3 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val3}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg2 + 1, enum3, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.set-size"][Instruction::Return]', {
      funcName: '[method]descriptor.set-size',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  const handleTable0 = [T_FLAG, 0];
  const captureTable0= new Map();
  let captureCnt0 = 0;
  handleTables[0] = handleTable0;
  
  function trampoline22(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable0[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable0.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Error$1.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="filesystem-error-code"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'filesystem-error-code');
    const ret = filesystemErrorCode(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="filesystem-error-code"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant4 = ret;
    if (variant4 === null || variant4=== undefined) {
      dataView(memory0).setInt8(arg1 + 0, 0, true);
    } else {
      const e = variant4;
      dataView(memory0).setInt8(arg1 + 0, 1, true);
      var val3 = e;
      let enum3;
      switch (val3) {
        case 'access': {
          enum3 = 0;
          break;
        }
        case 'would-block': {
          enum3 = 1;
          break;
        }
        case 'already': {
          enum3 = 2;
          break;
        }
        case 'bad-descriptor': {
          enum3 = 3;
          break;
        }
        case 'busy': {
          enum3 = 4;
          break;
        }
        case 'deadlock': {
          enum3 = 5;
          break;
        }
        case 'quota': {
          enum3 = 6;
          break;
        }
        case 'exist': {
          enum3 = 7;
          break;
        }
        case 'file-too-large': {
          enum3 = 8;
          break;
        }
        case 'illegal-byte-sequence': {
          enum3 = 9;
          break;
        }
        case 'in-progress': {
          enum3 = 10;
          break;
        }
        case 'interrupted': {
          enum3 = 11;
          break;
        }
        case 'invalid': {
          enum3 = 12;
          break;
        }
        case 'io': {
          enum3 = 13;
          break;
        }
        case 'is-directory': {
          enum3 = 14;
          break;
        }
        case 'loop': {
          enum3 = 15;
          break;
        }
        case 'too-many-links': {
          enum3 = 16;
          break;
        }
        case 'message-size': {
          enum3 = 17;
          break;
        }
        case 'name-too-long': {
          enum3 = 18;
          break;
        }
        case 'no-device': {
          enum3 = 19;
          break;
        }
        case 'no-entry': {
          enum3 = 20;
          break;
        }
        case 'no-lock': {
          enum3 = 21;
          break;
        }
        case 'insufficient-memory': {
          enum3 = 22;
          break;
        }
        case 'insufficient-space': {
          enum3 = 23;
          break;
        }
        case 'not-directory': {
          enum3 = 24;
          break;
        }
        case 'not-empty': {
          enum3 = 25;
          break;
        }
        case 'not-recoverable': {
          enum3 = 26;
          break;
        }
        case 'unsupported': {
          enum3 = 27;
          break;
        }
        case 'no-tty': {
          enum3 = 28;
          break;
        }
        case 'no-such-device': {
          enum3 = 29;
          break;
        }
        case 'overflow': {
          enum3 = 30;
          break;
        }
        case 'not-permitted': {
          enum3 = 31;
          break;
        }
        case 'pipe': {
          enum3 = 32;
          break;
        }
        case 'read-only': {
          enum3 = 33;
          break;
        }
        case 'invalid-seek': {
          enum3 = 34;
          break;
        }
        case 'text-file-busy': {
          enum3 = 35;
          break;
        }
        case 'cross-device': {
          enum3 = 36;
          break;
        }
        default: {
          if ((e) instanceof Error) {
            console.error(e);
          }
          
          throw new TypeError(`"${val3}" is not one of the cases of error-code`);
        }
      }
      dataView(memory0).setInt8(arg1 + 1, enum3, true);
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="filesystem-error-code"][Instruction::Return]', {
      funcName: 'filesystem-error-code',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline23(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.sync"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.sync');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.sync()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.sync"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant4 = ret;
    switch (variant4.tag) {
      case 'ok': {
        const e = variant4.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        break;
      }
      case 'err': {
        const e = variant4.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var val3 = e;
        let enum3;
        switch (val3) {
          case 'access': {
            enum3 = 0;
            break;
          }
          case 'would-block': {
            enum3 = 1;
            break;
          }
          case 'already': {
            enum3 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum3 = 3;
            break;
          }
          case 'busy': {
            enum3 = 4;
            break;
          }
          case 'deadlock': {
            enum3 = 5;
            break;
          }
          case 'quota': {
            enum3 = 6;
            break;
          }
          case 'exist': {
            enum3 = 7;
            break;
          }
          case 'file-too-large': {
            enum3 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum3 = 9;
            break;
          }
          case 'in-progress': {
            enum3 = 10;
            break;
          }
          case 'interrupted': {
            enum3 = 11;
            break;
          }
          case 'invalid': {
            enum3 = 12;
            break;
          }
          case 'io': {
            enum3 = 13;
            break;
          }
          case 'is-directory': {
            enum3 = 14;
            break;
          }
          case 'loop': {
            enum3 = 15;
            break;
          }
          case 'too-many-links': {
            enum3 = 16;
            break;
          }
          case 'message-size': {
            enum3 = 17;
            break;
          }
          case 'name-too-long': {
            enum3 = 18;
            break;
          }
          case 'no-device': {
            enum3 = 19;
            break;
          }
          case 'no-entry': {
            enum3 = 20;
            break;
          }
          case 'no-lock': {
            enum3 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum3 = 22;
            break;
          }
          case 'insufficient-space': {
            enum3 = 23;
            break;
          }
          case 'not-directory': {
            enum3 = 24;
            break;
          }
          case 'not-empty': {
            enum3 = 25;
            break;
          }
          case 'not-recoverable': {
            enum3 = 26;
            break;
          }
          case 'unsupported': {
            enum3 = 27;
            break;
          }
          case 'no-tty': {
            enum3 = 28;
            break;
          }
          case 'no-such-device': {
            enum3 = 29;
            break;
          }
          case 'overflow': {
            enum3 = 30;
            break;
          }
          case 'not-permitted': {
            enum3 = 31;
            break;
          }
          case 'pipe': {
            enum3 = 32;
            break;
          }
          case 'read-only': {
            enum3 = 33;
            break;
          }
          case 'invalid-seek': {
            enum3 = 34;
            break;
          }
          case 'text-file-busy': {
            enum3 = 35;
            break;
          }
          case 'cross-device': {
            enum3 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val3}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg1 + 1, enum3, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.sync"][Instruction::Return]', {
      funcName: '[method]descriptor.sync',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline24(arg0, arg1, arg2, arg3, arg4) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    if ((arg1 & 4294967294) !== 0) {
      throw new TypeError('flags have extraneous bits set');
    }
    var flags3 = {
      symlinkFollow: Boolean(arg1 & 1),
    };
    var ptr4 = arg2;
    var len4 = arg3;
    var result4 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr4, len4));
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.stat-at"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.stat-at');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.statAt(flags3, result4)};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.stat-at"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant14 = ret;
    switch (variant14.tag) {
      case 'ok': {
        const e = variant14.val;
        dataView(memory0).setInt8(arg4 + 0, 0, true);
        var {type: v5_0, linkCount: v5_1, size: v5_2, dataAccessTimestamp: v5_3, dataModificationTimestamp: v5_4, statusChangeTimestamp: v5_5 } = e;
        var val6 = v5_0;
        let enum6;
        switch (val6) {
          case 'unknown': {
            enum6 = 0;
            break;
          }
          case 'block-device': {
            enum6 = 1;
            break;
          }
          case 'character-device': {
            enum6 = 2;
            break;
          }
          case 'directory': {
            enum6 = 3;
            break;
          }
          case 'fifo': {
            enum6 = 4;
            break;
          }
          case 'symbolic-link': {
            enum6 = 5;
            break;
          }
          case 'regular-file': {
            enum6 = 6;
            break;
          }
          case 'socket': {
            enum6 = 7;
            break;
          }
          default: {
            if ((v5_0) instanceof Error) {
              console.error(v5_0);
            }
            
            throw new TypeError(`"${val6}" is not one of the cases of descriptor-type`);
          }
        }
        dataView(memory0).setInt8(arg4 + 8, enum6, true);
        dataView(memory0).setBigInt64(arg4 + 16, toUint64(v5_1), true);
        dataView(memory0).setBigInt64(arg4 + 24, toUint64(v5_2), true);
        var variant8 = v5_3;
        if (variant8 === null || variant8=== undefined) {
          dataView(memory0).setInt8(arg4 + 32, 0, true);
        } else {
          const e = variant8;
          dataView(memory0).setInt8(arg4 + 32, 1, true);
          var {seconds: v7_0, nanoseconds: v7_1 } = e;
          dataView(memory0).setBigInt64(arg4 + 40, toUint64(v7_0), true);
          dataView(memory0).setInt32(arg4 + 48, toUint32(v7_1), true);
        }
        var variant10 = v5_4;
        if (variant10 === null || variant10=== undefined) {
          dataView(memory0).setInt8(arg4 + 56, 0, true);
        } else {
          const e = variant10;
          dataView(memory0).setInt8(arg4 + 56, 1, true);
          var {seconds: v9_0, nanoseconds: v9_1 } = e;
          dataView(memory0).setBigInt64(arg4 + 64, toUint64(v9_0), true);
          dataView(memory0).setInt32(arg4 + 72, toUint32(v9_1), true);
        }
        var variant12 = v5_5;
        if (variant12 === null || variant12=== undefined) {
          dataView(memory0).setInt8(arg4 + 80, 0, true);
        } else {
          const e = variant12;
          dataView(memory0).setInt8(arg4 + 80, 1, true);
          var {seconds: v11_0, nanoseconds: v11_1 } = e;
          dataView(memory0).setBigInt64(arg4 + 88, toUint64(v11_0), true);
          dataView(memory0).setInt32(arg4 + 96, toUint32(v11_1), true);
        }
        break;
      }
      case 'err': {
        const e = variant14.val;
        dataView(memory0).setInt8(arg4 + 0, 1, true);
        var val13 = e;
        let enum13;
        switch (val13) {
          case 'access': {
            enum13 = 0;
            break;
          }
          case 'would-block': {
            enum13 = 1;
            break;
          }
          case 'already': {
            enum13 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum13 = 3;
            break;
          }
          case 'busy': {
            enum13 = 4;
            break;
          }
          case 'deadlock': {
            enum13 = 5;
            break;
          }
          case 'quota': {
            enum13 = 6;
            break;
          }
          case 'exist': {
            enum13 = 7;
            break;
          }
          case 'file-too-large': {
            enum13 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum13 = 9;
            break;
          }
          case 'in-progress': {
            enum13 = 10;
            break;
          }
          case 'interrupted': {
            enum13 = 11;
            break;
          }
          case 'invalid': {
            enum13 = 12;
            break;
          }
          case 'io': {
            enum13 = 13;
            break;
          }
          case 'is-directory': {
            enum13 = 14;
            break;
          }
          case 'loop': {
            enum13 = 15;
            break;
          }
          case 'too-many-links': {
            enum13 = 16;
            break;
          }
          case 'message-size': {
            enum13 = 17;
            break;
          }
          case 'name-too-long': {
            enum13 = 18;
            break;
          }
          case 'no-device': {
            enum13 = 19;
            break;
          }
          case 'no-entry': {
            enum13 = 20;
            break;
          }
          case 'no-lock': {
            enum13 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum13 = 22;
            break;
          }
          case 'insufficient-space': {
            enum13 = 23;
            break;
          }
          case 'not-directory': {
            enum13 = 24;
            break;
          }
          case 'not-empty': {
            enum13 = 25;
            break;
          }
          case 'not-recoverable': {
            enum13 = 26;
            break;
          }
          case 'unsupported': {
            enum13 = 27;
            break;
          }
          case 'no-tty': {
            enum13 = 28;
            break;
          }
          case 'no-such-device': {
            enum13 = 29;
            break;
          }
          case 'overflow': {
            enum13 = 30;
            break;
          }
          case 'not-permitted': {
            enum13 = 31;
            break;
          }
          case 'pipe': {
            enum13 = 32;
            break;
          }
          case 'read-only': {
            enum13 = 33;
            break;
          }
          case 'invalid-seek': {
            enum13 = 34;
            break;
          }
          case 'text-file-busy': {
            enum13 = 35;
            break;
          }
          case 'cross-device': {
            enum13 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val13}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg4 + 8, enum13, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.stat-at"][Instruction::Return]', {
      funcName: '[method]descriptor.stat-at',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline25(arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    if ((arg1 & 4294967294) !== 0) {
      throw new TypeError('flags have extraneous bits set');
    }
    var flags3 = {
      symlinkFollow: Boolean(arg1 & 1),
    };
    var ptr4 = arg2;
    var len4 = arg3;
    var result4 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr4, len4));
    if ((arg4 & 4294967280) !== 0) {
      throw new TypeError('flags have extraneous bits set');
    }
    var flags5 = {
      create: Boolean(arg4 & 1),
      directory: Boolean(arg4 & 2),
      exclusive: Boolean(arg4 & 4),
      truncate: Boolean(arg4 & 8),
    };
    if ((arg5 & 4294967232) !== 0) {
      throw new TypeError('flags have extraneous bits set');
    }
    var flags6 = {
      read: Boolean(arg5 & 1),
      write: Boolean(arg5 & 2),
      fileIntegritySync: Boolean(arg5 & 4),
      dataIntegritySync: Boolean(arg5 & 8),
      requestedWriteSync: Boolean(arg5 & 16),
      mutateDirectory: Boolean(arg5 & 32),
    };
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.open-at"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.open-at');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.openAt(flags3, result4, flags5, flags6)};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.open-at"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant9 = ret;
    switch (variant9.tag) {
      case 'ok': {
        const e = variant9.val;
        dataView(memory0).setInt8(arg6 + 0, 0, true);
        if (!(e instanceof Descriptor)) {
          throw new TypeError('Resource error: Not a valid "Descriptor" resource.');
        }
        var handle7 = e[symbolRscHandle];
        if (!handle7) {
          const rep = e[symbolRscRep] || ++captureCnt6;
          captureTable6.set(rep, e);
          handle7 = rscTableCreateOwn(handleTable6, rep);
        }
        dataView(memory0).setInt32(arg6 + 4, handle7, true);
        break;
      }
      case 'err': {
        const e = variant9.val;
        dataView(memory0).setInt8(arg6 + 0, 1, true);
        var val8 = e;
        let enum8;
        switch (val8) {
          case 'access': {
            enum8 = 0;
            break;
          }
          case 'would-block': {
            enum8 = 1;
            break;
          }
          case 'already': {
            enum8 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum8 = 3;
            break;
          }
          case 'busy': {
            enum8 = 4;
            break;
          }
          case 'deadlock': {
            enum8 = 5;
            break;
          }
          case 'quota': {
            enum8 = 6;
            break;
          }
          case 'exist': {
            enum8 = 7;
            break;
          }
          case 'file-too-large': {
            enum8 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum8 = 9;
            break;
          }
          case 'in-progress': {
            enum8 = 10;
            break;
          }
          case 'interrupted': {
            enum8 = 11;
            break;
          }
          case 'invalid': {
            enum8 = 12;
            break;
          }
          case 'io': {
            enum8 = 13;
            break;
          }
          case 'is-directory': {
            enum8 = 14;
            break;
          }
          case 'loop': {
            enum8 = 15;
            break;
          }
          case 'too-many-links': {
            enum8 = 16;
            break;
          }
          case 'message-size': {
            enum8 = 17;
            break;
          }
          case 'name-too-long': {
            enum8 = 18;
            break;
          }
          case 'no-device': {
            enum8 = 19;
            break;
          }
          case 'no-entry': {
            enum8 = 20;
            break;
          }
          case 'no-lock': {
            enum8 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum8 = 22;
            break;
          }
          case 'insufficient-space': {
            enum8 = 23;
            break;
          }
          case 'not-directory': {
            enum8 = 24;
            break;
          }
          case 'not-empty': {
            enum8 = 25;
            break;
          }
          case 'not-recoverable': {
            enum8 = 26;
            break;
          }
          case 'unsupported': {
            enum8 = 27;
            break;
          }
          case 'no-tty': {
            enum8 = 28;
            break;
          }
          case 'no-such-device': {
            enum8 = 29;
            break;
          }
          case 'overflow': {
            enum8 = 30;
            break;
          }
          case 'not-permitted': {
            enum8 = 31;
            break;
          }
          case 'pipe': {
            enum8 = 32;
            break;
          }
          case 'read-only': {
            enum8 = 33;
            break;
          }
          case 'invalid-seek': {
            enum8 = 34;
            break;
          }
          case 'text-file-busy': {
            enum8 = 35;
            break;
          }
          case 'cross-device': {
            enum8 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val8}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg6 + 4, enum8, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.open-at"][Instruction::Return]', {
      funcName: '[method]descriptor.open-at',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline26(arg0, arg1, arg2, arg3) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    var ptr3 = arg1;
    var len3 = arg2;
    var result3 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr3, len3));
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.unlink-file-at"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.unlink-file-at');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.unlinkFileAt(result3)};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.unlink-file-at"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg3 + 0, 0, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg3 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg3 + 1, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.unlink-file-at"][Instruction::Return]', {
      funcName: '[method]descriptor.unlink-file-at',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline27(arg0, arg1, arg2) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.read-via-stream"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.read-via-stream');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.readViaStream(BigInt.asUintN(64, arg1))};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.read-via-stream"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg2 + 0, 0, true);
        if (!(e instanceof InputStream)) {
          throw new TypeError('Resource error: Not a valid "InputStream" resource.');
        }
        var handle3 = e[symbolRscHandle];
        if (!handle3) {
          const rep = e[symbolRscRep] || ++captureCnt2;
          captureTable2.set(rep, e);
          handle3 = rscTableCreateOwn(handleTable2, rep);
        }
        dataView(memory0).setInt32(arg2 + 4, handle3, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg2 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg2 + 4, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.read-via-stream"][Instruction::Return]', {
      funcName: '[method]descriptor.read-via-stream',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline28(arg0, arg1, arg2) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.write-via-stream"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.write-via-stream');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.writeViaStream(BigInt.asUintN(64, arg1))};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.write-via-stream"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg2 + 0, 0, true);
        if (!(e instanceof OutputStream)) {
          throw new TypeError('Resource error: Not a valid "OutputStream" resource.');
        }
        var handle3 = e[symbolRscHandle];
        if (!handle3) {
          const rep = e[symbolRscRep] || ++captureCnt3;
          captureTable3.set(rep, e);
          handle3 = rscTableCreateOwn(handleTable3, rep);
        }
        dataView(memory0).setInt32(arg2 + 4, handle3, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg2 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg2 + 4, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.write-via-stream"][Instruction::Return]', {
      funcName: '[method]descriptor.write-via-stream',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline29(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.append-via-stream"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.append-via-stream');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.appendViaStream()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.append-via-stream"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        if (!(e instanceof OutputStream)) {
          throw new TypeError('Resource error: Not a valid "OutputStream" resource.');
        }
        var handle3 = e[symbolRscHandle];
        if (!handle3) {
          const rep = e[symbolRscRep] || ++captureCnt3;
          captureTable3.set(rep, e);
          handle3 = rscTableCreateOwn(handleTable3, rep);
        }
        dataView(memory0).setInt32(arg1 + 4, handle3, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg1 + 4, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.append-via-stream"][Instruction::Return]', {
      funcName: '[method]descriptor.append-via-stream',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline30(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.get-type"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.get-type');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.getType()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.get-type"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        var val3 = e;
        let enum3;
        switch (val3) {
          case 'unknown': {
            enum3 = 0;
            break;
          }
          case 'block-device': {
            enum3 = 1;
            break;
          }
          case 'character-device': {
            enum3 = 2;
            break;
          }
          case 'directory': {
            enum3 = 3;
            break;
          }
          case 'fifo': {
            enum3 = 4;
            break;
          }
          case 'symbolic-link': {
            enum3 = 5;
            break;
          }
          case 'regular-file': {
            enum3 = 6;
            break;
          }
          case 'socket': {
            enum3 = 7;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val3}" is not one of the cases of descriptor-type`);
          }
        }
        dataView(memory0).setInt8(arg1 + 1, enum3, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg1 + 1, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.get-type"][Instruction::Return]', {
      funcName: '[method]descriptor.get-type',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline31(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.stat"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.stat');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.stat()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.stat"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant12 = ret;
    switch (variant12.tag) {
      case 'ok': {
        const e = variant12.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        var {type: v3_0, linkCount: v3_1, size: v3_2, dataAccessTimestamp: v3_3, dataModificationTimestamp: v3_4, statusChangeTimestamp: v3_5 } = e;
        var val4 = v3_0;
        let enum4;
        switch (val4) {
          case 'unknown': {
            enum4 = 0;
            break;
          }
          case 'block-device': {
            enum4 = 1;
            break;
          }
          case 'character-device': {
            enum4 = 2;
            break;
          }
          case 'directory': {
            enum4 = 3;
            break;
          }
          case 'fifo': {
            enum4 = 4;
            break;
          }
          case 'symbolic-link': {
            enum4 = 5;
            break;
          }
          case 'regular-file': {
            enum4 = 6;
            break;
          }
          case 'socket': {
            enum4 = 7;
            break;
          }
          default: {
            if ((v3_0) instanceof Error) {
              console.error(v3_0);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of descriptor-type`);
          }
        }
        dataView(memory0).setInt8(arg1 + 8, enum4, true);
        dataView(memory0).setBigInt64(arg1 + 16, toUint64(v3_1), true);
        dataView(memory0).setBigInt64(arg1 + 24, toUint64(v3_2), true);
        var variant6 = v3_3;
        if (variant6 === null || variant6=== undefined) {
          dataView(memory0).setInt8(arg1 + 32, 0, true);
        } else {
          const e = variant6;
          dataView(memory0).setInt8(arg1 + 32, 1, true);
          var {seconds: v5_0, nanoseconds: v5_1 } = e;
          dataView(memory0).setBigInt64(arg1 + 40, toUint64(v5_0), true);
          dataView(memory0).setInt32(arg1 + 48, toUint32(v5_1), true);
        }
        var variant8 = v3_4;
        if (variant8 === null || variant8=== undefined) {
          dataView(memory0).setInt8(arg1 + 56, 0, true);
        } else {
          const e = variant8;
          dataView(memory0).setInt8(arg1 + 56, 1, true);
          var {seconds: v7_0, nanoseconds: v7_1 } = e;
          dataView(memory0).setBigInt64(arg1 + 64, toUint64(v7_0), true);
          dataView(memory0).setInt32(arg1 + 72, toUint32(v7_1), true);
        }
        var variant10 = v3_5;
        if (variant10 === null || variant10=== undefined) {
          dataView(memory0).setInt8(arg1 + 80, 0, true);
        } else {
          const e = variant10;
          dataView(memory0).setInt8(arg1 + 80, 1, true);
          var {seconds: v9_0, nanoseconds: v9_1 } = e;
          dataView(memory0).setBigInt64(arg1 + 88, toUint64(v9_0), true);
          dataView(memory0).setInt32(arg1 + 96, toUint32(v9_1), true);
        }
        break;
      }
      case 'err': {
        const e = variant12.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var val11 = e;
        let enum11;
        switch (val11) {
          case 'access': {
            enum11 = 0;
            break;
          }
          case 'would-block': {
            enum11 = 1;
            break;
          }
          case 'already': {
            enum11 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum11 = 3;
            break;
          }
          case 'busy': {
            enum11 = 4;
            break;
          }
          case 'deadlock': {
            enum11 = 5;
            break;
          }
          case 'quota': {
            enum11 = 6;
            break;
          }
          case 'exist': {
            enum11 = 7;
            break;
          }
          case 'file-too-large': {
            enum11 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum11 = 9;
            break;
          }
          case 'in-progress': {
            enum11 = 10;
            break;
          }
          case 'interrupted': {
            enum11 = 11;
            break;
          }
          case 'invalid': {
            enum11 = 12;
            break;
          }
          case 'io': {
            enum11 = 13;
            break;
          }
          case 'is-directory': {
            enum11 = 14;
            break;
          }
          case 'loop': {
            enum11 = 15;
            break;
          }
          case 'too-many-links': {
            enum11 = 16;
            break;
          }
          case 'message-size': {
            enum11 = 17;
            break;
          }
          case 'name-too-long': {
            enum11 = 18;
            break;
          }
          case 'no-device': {
            enum11 = 19;
            break;
          }
          case 'no-entry': {
            enum11 = 20;
            break;
          }
          case 'no-lock': {
            enum11 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum11 = 22;
            break;
          }
          case 'insufficient-space': {
            enum11 = 23;
            break;
          }
          case 'not-directory': {
            enum11 = 24;
            break;
          }
          case 'not-empty': {
            enum11 = 25;
            break;
          }
          case 'not-recoverable': {
            enum11 = 26;
            break;
          }
          case 'unsupported': {
            enum11 = 27;
            break;
          }
          case 'no-tty': {
            enum11 = 28;
            break;
          }
          case 'no-such-device': {
            enum11 = 29;
            break;
          }
          case 'overflow': {
            enum11 = 30;
            break;
          }
          case 'not-permitted': {
            enum11 = 31;
            break;
          }
          case 'pipe': {
            enum11 = 32;
            break;
          }
          case 'read-only': {
            enum11 = 33;
            break;
          }
          case 'invalid-seek': {
            enum11 = 34;
            break;
          }
          case 'text-file-busy': {
            enum11 = 35;
            break;
          }
          case 'cross-device': {
            enum11 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val11}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg1 + 8, enum11, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.stat"][Instruction::Return]', {
      funcName: '[method]descriptor.stat',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline32(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.metadata-hash"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.metadata-hash');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.metadataHash()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.metadata-hash"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        var {lower: v3_0, upper: v3_1 } = e;
        dataView(memory0).setBigInt64(arg1 + 8, toUint64(v3_0), true);
        dataView(memory0).setBigInt64(arg1 + 16, toUint64(v3_1), true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var val4 = e;
        let enum4;
        switch (val4) {
          case 'access': {
            enum4 = 0;
            break;
          }
          case 'would-block': {
            enum4 = 1;
            break;
          }
          case 'already': {
            enum4 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum4 = 3;
            break;
          }
          case 'busy': {
            enum4 = 4;
            break;
          }
          case 'deadlock': {
            enum4 = 5;
            break;
          }
          case 'quota': {
            enum4 = 6;
            break;
          }
          case 'exist': {
            enum4 = 7;
            break;
          }
          case 'file-too-large': {
            enum4 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum4 = 9;
            break;
          }
          case 'in-progress': {
            enum4 = 10;
            break;
          }
          case 'interrupted': {
            enum4 = 11;
            break;
          }
          case 'invalid': {
            enum4 = 12;
            break;
          }
          case 'io': {
            enum4 = 13;
            break;
          }
          case 'is-directory': {
            enum4 = 14;
            break;
          }
          case 'loop': {
            enum4 = 15;
            break;
          }
          case 'too-many-links': {
            enum4 = 16;
            break;
          }
          case 'message-size': {
            enum4 = 17;
            break;
          }
          case 'name-too-long': {
            enum4 = 18;
            break;
          }
          case 'no-device': {
            enum4 = 19;
            break;
          }
          case 'no-entry': {
            enum4 = 20;
            break;
          }
          case 'no-lock': {
            enum4 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum4 = 22;
            break;
          }
          case 'insufficient-space': {
            enum4 = 23;
            break;
          }
          case 'not-directory': {
            enum4 = 24;
            break;
          }
          case 'not-empty': {
            enum4 = 25;
            break;
          }
          case 'not-recoverable': {
            enum4 = 26;
            break;
          }
          case 'unsupported': {
            enum4 = 27;
            break;
          }
          case 'no-tty': {
            enum4 = 28;
            break;
          }
          case 'no-such-device': {
            enum4 = 29;
            break;
          }
          case 'overflow': {
            enum4 = 30;
            break;
          }
          case 'not-permitted': {
            enum4 = 31;
            break;
          }
          case 'pipe': {
            enum4 = 32;
            break;
          }
          case 'read-only': {
            enum4 = 33;
            break;
          }
          case 'invalid-seek': {
            enum4 = 34;
            break;
          }
          case 'text-file-busy': {
            enum4 = 35;
            break;
          }
          case 'cross-device': {
            enum4 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val4}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg1 + 8, enum4, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.metadata-hash"][Instruction::Return]', {
      funcName: '[method]descriptor.metadata-hash',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline33(arg0, arg1, arg2, arg3, arg4) {
    var handle1 = arg0;
    var rep2 = handleTable6[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable6.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(Descriptor.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    if ((arg1 & 4294967294) !== 0) {
      throw new TypeError('flags have extraneous bits set');
    }
    var flags3 = {
      symlinkFollow: Boolean(arg1 & 1),
    };
    var ptr4 = arg2;
    var len4 = arg3;
    var result4 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr4, len4));
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.metadata-hash-at"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]descriptor.metadata-hash-at');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.metadataHashAt(flags3, result4)};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.metadata-hash-at"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant7 = ret;
    switch (variant7.tag) {
      case 'ok': {
        const e = variant7.val;
        dataView(memory0).setInt8(arg4 + 0, 0, true);
        var {lower: v5_0, upper: v5_1 } = e;
        dataView(memory0).setBigInt64(arg4 + 8, toUint64(v5_0), true);
        dataView(memory0).setBigInt64(arg4 + 16, toUint64(v5_1), true);
        break;
      }
      case 'err': {
        const e = variant7.val;
        dataView(memory0).setInt8(arg4 + 0, 1, true);
        var val6 = e;
        let enum6;
        switch (val6) {
          case 'access': {
            enum6 = 0;
            break;
          }
          case 'would-block': {
            enum6 = 1;
            break;
          }
          case 'already': {
            enum6 = 2;
            break;
          }
          case 'bad-descriptor': {
            enum6 = 3;
            break;
          }
          case 'busy': {
            enum6 = 4;
            break;
          }
          case 'deadlock': {
            enum6 = 5;
            break;
          }
          case 'quota': {
            enum6 = 6;
            break;
          }
          case 'exist': {
            enum6 = 7;
            break;
          }
          case 'file-too-large': {
            enum6 = 8;
            break;
          }
          case 'illegal-byte-sequence': {
            enum6 = 9;
            break;
          }
          case 'in-progress': {
            enum6 = 10;
            break;
          }
          case 'interrupted': {
            enum6 = 11;
            break;
          }
          case 'invalid': {
            enum6 = 12;
            break;
          }
          case 'io': {
            enum6 = 13;
            break;
          }
          case 'is-directory': {
            enum6 = 14;
            break;
          }
          case 'loop': {
            enum6 = 15;
            break;
          }
          case 'too-many-links': {
            enum6 = 16;
            break;
          }
          case 'message-size': {
            enum6 = 17;
            break;
          }
          case 'name-too-long': {
            enum6 = 18;
            break;
          }
          case 'no-device': {
            enum6 = 19;
            break;
          }
          case 'no-entry': {
            enum6 = 20;
            break;
          }
          case 'no-lock': {
            enum6 = 21;
            break;
          }
          case 'insufficient-memory': {
            enum6 = 22;
            break;
          }
          case 'insufficient-space': {
            enum6 = 23;
            break;
          }
          case 'not-directory': {
            enum6 = 24;
            break;
          }
          case 'not-empty': {
            enum6 = 25;
            break;
          }
          case 'not-recoverable': {
            enum6 = 26;
            break;
          }
          case 'unsupported': {
            enum6 = 27;
            break;
          }
          case 'no-tty': {
            enum6 = 28;
            break;
          }
          case 'no-such-device': {
            enum6 = 29;
            break;
          }
          case 'overflow': {
            enum6 = 30;
            break;
          }
          case 'not-permitted': {
            enum6 = 31;
            break;
          }
          case 'pipe': {
            enum6 = 32;
            break;
          }
          case 'read-only': {
            enum6 = 33;
            break;
          }
          case 'invalid-seek': {
            enum6 = 34;
            break;
          }
          case 'text-file-busy': {
            enum6 = 35;
            break;
          }
          case 'cross-device': {
            enum6 = 36;
            break;
          }
          default: {
            if ((e) instanceof Error) {
              console.error(e);
            }
            
            throw new TypeError(`"${val6}" is not one of the cases of error-code`);
          }
        }
        dataView(memory0).setInt8(arg4 + 8, enum6, true);
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:filesystem/types@0.2.0", function="[method]descriptor.metadata-hash-at"][Instruction::Return]', {
      funcName: '[method]descriptor.metadata-hash-at',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline34(arg0, arg1, arg2) {
    var handle1 = arg0;
    var rep2 = handleTable2[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable2.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(InputStream.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]input-stream.read"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]input-stream.read');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.read(BigInt.asUintN(64, arg1))};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]input-stream.read"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant6 = ret;
    switch (variant6.tag) {
      case 'ok': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg2 + 0, 0, true);
        var val3 = e;
        var len3 = val3.byteLength;
        var ptr3 = realloc0(0, 0, 1, len3 * 1);
        var src3 = new Uint8Array(val3.buffer || val3, val3.byteOffset, len3 * 1);
        (new Uint8Array(memory0.buffer, ptr3, len3 * 1)).set(src3);
        dataView(memory0).setUint32(arg2 + 8, len3, true);
        dataView(memory0).setUint32(arg2 + 4, ptr3, true);
        break;
      }
      case 'err': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg2 + 0, 1, true);
        var variant5 = e;
        switch (variant5.tag) {
          case 'last-operation-failed': {
            const e = variant5.val;
            dataView(memory0).setInt8(arg2 + 4, 0, true);
            if (!(e instanceof Error$1)) {
              throw new TypeError('Resource error: Not a valid "Error" resource.');
            }
            var handle4 = e[symbolRscHandle];
            if (!handle4) {
              const rep = e[symbolRscRep] || ++captureCnt0;
              captureTable0.set(rep, e);
              handle4 = rscTableCreateOwn(handleTable0, rep);
            }
            dataView(memory0).setInt32(arg2 + 8, handle4, true);
            break;
          }
          case 'closed': {
            dataView(memory0).setInt8(arg2 + 4, 1, true);
            break;
          }
          default: {
            throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant5.tag)}\` (received \`${variant5}\`) specified for \`StreamError\``);
          }
        }
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]input-stream.read"][Instruction::Return]', {
      funcName: '[method]input-stream.read',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline35(arg0, arg1, arg2) {
    var handle1 = arg0;
    var rep2 = handleTable2[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable2.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(InputStream.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]input-stream.blocking-read"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]input-stream.blocking-read');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.blockingRead(BigInt.asUintN(64, arg1))};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]input-stream.blocking-read"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant6 = ret;
    switch (variant6.tag) {
      case 'ok': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg2 + 0, 0, true);
        var val3 = e;
        var len3 = val3.byteLength;
        var ptr3 = realloc0(0, 0, 1, len3 * 1);
        var src3 = new Uint8Array(val3.buffer || val3, val3.byteOffset, len3 * 1);
        (new Uint8Array(memory0.buffer, ptr3, len3 * 1)).set(src3);
        dataView(memory0).setUint32(arg2 + 8, len3, true);
        dataView(memory0).setUint32(arg2 + 4, ptr3, true);
        break;
      }
      case 'err': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg2 + 0, 1, true);
        var variant5 = e;
        switch (variant5.tag) {
          case 'last-operation-failed': {
            const e = variant5.val;
            dataView(memory0).setInt8(arg2 + 4, 0, true);
            if (!(e instanceof Error$1)) {
              throw new TypeError('Resource error: Not a valid "Error" resource.');
            }
            var handle4 = e[symbolRscHandle];
            if (!handle4) {
              const rep = e[symbolRscRep] || ++captureCnt0;
              captureTable0.set(rep, e);
              handle4 = rscTableCreateOwn(handleTable0, rep);
            }
            dataView(memory0).setInt32(arg2 + 8, handle4, true);
            break;
          }
          case 'closed': {
            dataView(memory0).setInt8(arg2 + 4, 1, true);
            break;
          }
          default: {
            throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant5.tag)}\` (received \`${variant5}\`) specified for \`StreamError\``);
          }
        }
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]input-stream.blocking-read"][Instruction::Return]', {
      funcName: '[method]input-stream.blocking-read',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline36(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable3[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable3.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(OutputStream.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.check-write"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]output-stream.check-write');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.checkWrite()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.check-write"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        dataView(memory0).setBigInt64(arg1 + 8, toUint64(e), true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var variant4 = e;
        switch (variant4.tag) {
          case 'last-operation-failed': {
            const e = variant4.val;
            dataView(memory0).setInt8(arg1 + 8, 0, true);
            if (!(e instanceof Error$1)) {
              throw new TypeError('Resource error: Not a valid "Error" resource.');
            }
            var handle3 = e[symbolRscHandle];
            if (!handle3) {
              const rep = e[symbolRscRep] || ++captureCnt0;
              captureTable0.set(rep, e);
              handle3 = rscTableCreateOwn(handleTable0, rep);
            }
            dataView(memory0).setInt32(arg1 + 12, handle3, true);
            break;
          }
          case 'closed': {
            dataView(memory0).setInt8(arg1 + 8, 1, true);
            break;
          }
          default: {
            throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant4.tag)}\` (received \`${variant4}\`) specified for \`StreamError\``);
          }
        }
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.check-write"][Instruction::Return]', {
      funcName: '[method]output-stream.check-write',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline37(arg0, arg1, arg2, arg3) {
    var handle1 = arg0;
    var rep2 = handleTable3[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable3.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(OutputStream.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    var ptr3 = arg1;
    var len3 = arg2;
    var result3 = new Uint8Array(memory0.buffer.slice(ptr3, ptr3 + len3 * 1));
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.write"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]output-stream.write');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.write(result3)};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.write"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant6 = ret;
    switch (variant6.tag) {
      case 'ok': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg3 + 0, 0, true);
        break;
      }
      case 'err': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg3 + 0, 1, true);
        var variant5 = e;
        switch (variant5.tag) {
          case 'last-operation-failed': {
            const e = variant5.val;
            dataView(memory0).setInt8(arg3 + 4, 0, true);
            if (!(e instanceof Error$1)) {
              throw new TypeError('Resource error: Not a valid "Error" resource.');
            }
            var handle4 = e[symbolRscHandle];
            if (!handle4) {
              const rep = e[symbolRscRep] || ++captureCnt0;
              captureTable0.set(rep, e);
              handle4 = rscTableCreateOwn(handleTable0, rep);
            }
            dataView(memory0).setInt32(arg3 + 8, handle4, true);
            break;
          }
          case 'closed': {
            dataView(memory0).setInt8(arg3 + 4, 1, true);
            break;
          }
          default: {
            throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant5.tag)}\` (received \`${variant5}\`) specified for \`StreamError\``);
          }
        }
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.write"][Instruction::Return]', {
      funcName: '[method]output-stream.write',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline38(arg0, arg1) {
    var handle1 = arg0;
    var rep2 = handleTable3[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable3.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(OutputStream.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.blocking-flush"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]output-stream.blocking-flush');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.blockingFlush()};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.blocking-flush"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant5 = ret;
    switch (variant5.tag) {
      case 'ok': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 0, true);
        break;
      }
      case 'err': {
        const e = variant5.val;
        dataView(memory0).setInt8(arg1 + 0, 1, true);
        var variant4 = e;
        switch (variant4.tag) {
          case 'last-operation-failed': {
            const e = variant4.val;
            dataView(memory0).setInt8(arg1 + 4, 0, true);
            if (!(e instanceof Error$1)) {
              throw new TypeError('Resource error: Not a valid "Error" resource.');
            }
            var handle3 = e[symbolRscHandle];
            if (!handle3) {
              const rep = e[symbolRscRep] || ++captureCnt0;
              captureTable0.set(rep, e);
              handle3 = rscTableCreateOwn(handleTable0, rep);
            }
            dataView(memory0).setInt32(arg1 + 8, handle3, true);
            break;
          }
          case 'closed': {
            dataView(memory0).setInt8(arg1 + 4, 1, true);
            break;
          }
          default: {
            throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant4.tag)}\` (received \`${variant4}\`) specified for \`StreamError\``);
          }
        }
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.blocking-flush"][Instruction::Return]', {
      funcName: '[method]output-stream.blocking-flush',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline39(arg0, arg1, arg2, arg3) {
    var handle1 = arg0;
    var rep2 = handleTable3[(handle1 << 1) + 1] & ~T_FLAG;
    var rsc0 = captureTable3.get(rep2);
    if (!rsc0) {
      rsc0 = Object.create(OutputStream.prototype);
      Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
      Object.defineProperty(rsc0, symbolRscRep, { writable: true, value: rep2});
    }
    curResourceBorrows.push(rsc0);
    var ptr3 = arg1;
    var len3 = arg2;
    var result3 = new Uint8Array(memory0.buffer.slice(ptr3, ptr3 + len3 * 1));
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.blocking-write-and-flush"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, '[method]output-stream.blocking-write-and-flush');
    let ret;
    try {
      ret = { tag: 'ok', val: rsc0.blockingWriteAndFlush(result3)};
    } catch (e) {
      ret = { tag: 'err', val: getErrorPayload(e) };
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.blocking-write-and-flush"] [Instruction::CallInterface] (sync, @ post-call)');
    for (const rsc of curResourceBorrows) {
      rsc[symbolRscHandle] = undefined;
    }
    curResourceBorrows = [];
    endCurrentTask(0);
    var variant6 = ret;
    switch (variant6.tag) {
      case 'ok': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg3 + 0, 0, true);
        break;
      }
      case 'err': {
        const e = variant6.val;
        dataView(memory0).setInt8(arg3 + 0, 1, true);
        var variant5 = e;
        switch (variant5.tag) {
          case 'last-operation-failed': {
            const e = variant5.val;
            dataView(memory0).setInt8(arg3 + 4, 0, true);
            if (!(e instanceof Error$1)) {
              throw new TypeError('Resource error: Not a valid "Error" resource.');
            }
            var handle4 = e[symbolRscHandle];
            if (!handle4) {
              const rep = e[symbolRscRep] || ++captureCnt0;
              captureTable0.set(rep, e);
              handle4 = rscTableCreateOwn(handleTable0, rep);
            }
            dataView(memory0).setInt32(arg3 + 8, handle4, true);
            break;
          }
          case 'closed': {
            dataView(memory0).setInt8(arg3 + 4, 1, true);
            break;
          }
          default: {
            throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant5.tag)}\` (received \`${variant5}\`) specified for \`StreamError\``);
          }
        }
        break;
      }
      default: {
        throw new TypeError('invalid variant specified for result');
      }
    }
    _debugLog('[iface="wasi:io/streams@0.2.0", function="[method]output-stream.blocking-write-and-flush"][Instruction::Return]', {
      funcName: '[method]output-stream.blocking-write-and-flush',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline40(arg0) {
    _debugLog('[iface="wasi:filesystem/preopens@0.2.0", function="get-directories"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-directories');
    const ret = getDirectories();
    _debugLog('[iface="wasi:filesystem/preopens@0.2.0", function="get-directories"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    var vec3 = ret;
    var len3 = vec3.length;
    var result3 = realloc0(0, 0, 4, len3 * 12);
    for (let i = 0; i < vec3.length; i++) {
      const e = vec3[i];
      const base = result3 + i * 12;var [tuple0_0, tuple0_1] = e;
      if (!(tuple0_0 instanceof Descriptor)) {
        throw new TypeError('Resource error: Not a valid "Descriptor" resource.');
      }
      var handle1 = tuple0_0[symbolRscHandle];
      if (!handle1) {
        const rep = tuple0_0[symbolRscRep] || ++captureCnt6;
        captureTable6.set(rep, tuple0_0);
        handle1 = rscTableCreateOwn(handleTable6, rep);
      }
      dataView(memory0).setInt32(base + 0, handle1, true);
      var ptr2 = utf8Encode(tuple0_1, realloc0, memory0);
      var len2 = utf8EncodedLen;
      dataView(memory0).setUint32(base + 8, len2, true);
      dataView(memory0).setUint32(base + 4, ptr2, true);
    }
    dataView(memory0).setUint32(arg0 + 4, len3, true);
    dataView(memory0).setUint32(arg0 + 0, result3, true);
    _debugLog('[iface="wasi:filesystem/preopens@0.2.0", function="get-directories"][Instruction::Return]', {
      funcName: 'get-directories',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  const handleTable4 = [T_FLAG, 0];
  const captureTable4= new Map();
  let captureCnt4 = 0;
  handleTables[4] = handleTable4;
  
  function trampoline41(arg0) {
    _debugLog('[iface="wasi:cli/terminal-stdin@0.2.0", function="get-terminal-stdin"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-terminal-stdin');
    const ret = getTerminalStdin();
    _debugLog('[iface="wasi:cli/terminal-stdin@0.2.0", function="get-terminal-stdin"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    var variant1 = ret;
    if (variant1 === null || variant1=== undefined) {
      dataView(memory0).setInt8(arg0 + 0, 0, true);
    } else {
      const e = variant1;
      dataView(memory0).setInt8(arg0 + 0, 1, true);
      if (!(e instanceof TerminalInput)) {
        throw new TypeError('Resource error: Not a valid "TerminalInput" resource.');
      }
      var handle0 = e[symbolRscHandle];
      if (!handle0) {
        const rep = e[symbolRscRep] || ++captureCnt4;
        captureTable4.set(rep, e);
        handle0 = rscTableCreateOwn(handleTable4, rep);
      }
      dataView(memory0).setInt32(arg0 + 4, handle0, true);
    }
    _debugLog('[iface="wasi:cli/terminal-stdin@0.2.0", function="get-terminal-stdin"][Instruction::Return]', {
      funcName: 'get-terminal-stdin',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  const handleTable5 = [T_FLAG, 0];
  const captureTable5= new Map();
  let captureCnt5 = 0;
  handleTables[5] = handleTable5;
  
  function trampoline42(arg0) {
    _debugLog('[iface="wasi:cli/terminal-stdout@0.2.0", function="get-terminal-stdout"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-terminal-stdout');
    const ret = getTerminalStdout();
    _debugLog('[iface="wasi:cli/terminal-stdout@0.2.0", function="get-terminal-stdout"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    var variant1 = ret;
    if (variant1 === null || variant1=== undefined) {
      dataView(memory0).setInt8(arg0 + 0, 0, true);
    } else {
      const e = variant1;
      dataView(memory0).setInt8(arg0 + 0, 1, true);
      if (!(e instanceof TerminalOutput)) {
        throw new TypeError('Resource error: Not a valid "TerminalOutput" resource.');
      }
      var handle0 = e[symbolRscHandle];
      if (!handle0) {
        const rep = e[symbolRscRep] || ++captureCnt5;
        captureTable5.set(rep, e);
        handle0 = rscTableCreateOwn(handleTable5, rep);
      }
      dataView(memory0).setInt32(arg0 + 4, handle0, true);
    }
    _debugLog('[iface="wasi:cli/terminal-stdout@0.2.0", function="get-terminal-stdout"][Instruction::Return]', {
      funcName: 'get-terminal-stdout',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  
  function trampoline43(arg0) {
    _debugLog('[iface="wasi:cli/terminal-stderr@0.2.0", function="get-terminal-stderr"] [Instruction::CallInterface] (async? sync, @ enter)');
    const _interface_call_currentTaskID = startCurrentTask(0, false, 'get-terminal-stderr');
    const ret = getTerminalStderr();
    _debugLog('[iface="wasi:cli/terminal-stderr@0.2.0", function="get-terminal-stderr"] [Instruction::CallInterface] (sync, @ post-call)');
    endCurrentTask(0);
    var variant1 = ret;
    if (variant1 === null || variant1=== undefined) {
      dataView(memory0).setInt8(arg0 + 0, 0, true);
    } else {
      const e = variant1;
      dataView(memory0).setInt8(arg0 + 0, 1, true);
      if (!(e instanceof TerminalOutput)) {
        throw new TypeError('Resource error: Not a valid "TerminalOutput" resource.');
      }
      var handle0 = e[symbolRscHandle];
      if (!handle0) {
        const rep = e[symbolRscRep] || ++captureCnt5;
        captureTable5.set(rep, e);
        handle0 = rscTableCreateOwn(handleTable5, rep);
      }
      dataView(memory0).setInt32(arg0 + 4, handle0, true);
    }
    _debugLog('[iface="wasi:cli/terminal-stderr@0.2.0", function="get-terminal-stderr"][Instruction::Return]', {
      funcName: 'get-terminal-stderr',
      paramCount: 0,
      async: false,
      postReturn: false
    });
  }
  
  let exports3;
  let exports4;
  let realloc1;
  let postReturn0;
  let postReturn1;
  let postReturn2;
  let postReturn3;
  let postReturn4;
  let postReturn5;
  let postReturn6;
  let postReturn7;
  let postReturn8;
  let postReturn9;
  let postReturn10;
  let postReturn11;
  let postReturn12;
  let postReturn13;
  let postReturn14;
  let postReturn15;
  let postReturn16;
  let postReturn17;
  let postReturn18;
  let postReturn19;
  let postReturn20;
  let postReturn21;
  let postReturn22;
  let postReturn23;
  let postReturn24;
  let postReturn25;
  let postReturn26;
  const handleTable12 = [T_FLAG, 0];
  const finalizationRegistry12 = finalizationRegistryCreate((handle) => {
    const { rep } = rscTableRemove(handleTable12, handle);
    exports0['16'](rep);
  });
  
  handleTables[12] = handleTable12;
  const trampoline0 = rscTableCreateOwn.bind(null, handleTable12);
  const handleTable13 = [T_FLAG, 0];
  const finalizationRegistry13 = finalizationRegistryCreate((handle) => {
    const { rep } = rscTableRemove(handleTable13, handle);
    exports0['17'](rep);
  });
  
  handleTables[13] = handleTable13;
  const trampoline1 = rscTableCreateOwn.bind(null, handleTable13);
  const handleTable1 = [T_FLAG, 0];
  const captureTable1= new Map();
  let captureCnt1 = 0;
  handleTables[1] = handleTable1;
  function trampoline2(handle) {
    const handleEntry = rscTableRemove(handleTable1, handle);
    if (handleEntry.own) {
      throw new TypeError('unreachable trampoline for resource [ResourceIndex(1)]')
    }
  }
  function trampoline3(handle) {
    const handleEntry = rscTableRemove(handleTable2, handle);
    if (handleEntry.own) {
      
      const rsc = captureTable2.get(handleEntry.rep);
      if (rsc) {
        if (rsc[symbolDispose]) rsc[symbolDispose]();
        captureTable2.delete(handleEntry.rep);
      } else if (InputStream[symbolCabiDispose]) {
        InputStream[symbolCabiDispose](handleEntry.rep);
      }
    }
  }
  function trampoline4(handle) {
    const handleEntry = rscTableRemove(handleTable3, handle);
    if (handleEntry.own) {
      
      const rsc = captureTable3.get(handleEntry.rep);
      if (rsc) {
        if (rsc[symbolDispose]) rsc[symbolDispose]();
        captureTable3.delete(handleEntry.rep);
      } else if (OutputStream[symbolCabiDispose]) {
        OutputStream[symbolCabiDispose](handleEntry.rep);
      }
    }
  }
  const handleTable8 = [T_FLAG, 0];
  const captureTable8= new Map();
  let captureCnt8 = 0;
  handleTables[8] = handleTable8;
  function trampoline5(handle) {
    const handleEntry = rscTableRemove(handleTable8, handle);
    if (handleEntry.own) {
      throw new TypeError('unreachable trampoline for resource [ResourceIndex(8)]')
    }
  }
  const handleTable9 = [T_FLAG, 0];
  const captureTable9= new Map();
  let captureCnt9 = 0;
  handleTables[9] = handleTable9;
  function trampoline6(handle) {
    const handleEntry = rscTableRemove(handleTable9, handle);
    if (handleEntry.own) {
      throw new TypeError('unreachable trampoline for resource [ResourceIndex(9)]')
    }
  }
  const handleTable10 = [T_FLAG, 0];
  const captureTable10= new Map();
  let captureCnt10 = 0;
  handleTables[10] = handleTable10;
  function trampoline7(handle) {
    const handleEntry = rscTableRemove(handleTable10, handle);
    if (handleEntry.own) {
      throw new TypeError('unreachable trampoline for resource [ResourceIndex(10)]')
    }
  }
  const handleTable11 = [T_FLAG, 0];
  const captureTable11= new Map();
  let captureCnt11 = 0;
  handleTables[11] = handleTable11;
  function trampoline8(handle) {
    const handleEntry = rscTableRemove(handleTable11, handle);
    if (handleEntry.own) {
      throw new TypeError('unreachable trampoline for resource [ResourceIndex(11)]')
    }
  }
  const handleTable7 = [T_FLAG, 0];
  const captureTable7= new Map();
  let captureCnt7 = 0;
  handleTables[7] = handleTable7;
  function trampoline10(handle) {
    const handleEntry = rscTableRemove(handleTable7, handle);
    if (handleEntry.own) {
      throw new TypeError('unreachable trampoline for resource [ResourceIndex(7)]')
    }
  }
  function trampoline11(handle) {
    const handleEntry = rscTableRemove(handleTable6, handle);
    if (handleEntry.own) {
      
      const rsc = captureTable6.get(handleEntry.rep);
      if (rsc) {
        if (rsc[symbolDispose]) rsc[symbolDispose]();
        captureTable6.delete(handleEntry.rep);
      } else if (Descriptor[symbolCabiDispose]) {
        Descriptor[symbolCabiDispose](handleEntry.rep);
      }
    }
  }
  function trampoline12(handle) {
    const handleEntry = rscTableRemove(handleTable0, handle);
    if (handleEntry.own) {
      
      const rsc = captureTable0.get(handleEntry.rep);
      if (rsc) {
        if (rsc[symbolDispose]) rsc[symbolDispose]();
        captureTable0.delete(handleEntry.rep);
      } else if (Error$1[symbolCabiDispose]) {
        Error$1[symbolCabiDispose](handleEntry.rep);
      }
    }
  }
  function trampoline14(handle) {
    const handleEntry = rscTableRemove(handleTable4, handle);
    if (handleEntry.own) {
      
      const rsc = captureTable4.get(handleEntry.rep);
      if (rsc) {
        if (rsc[symbolDispose]) rsc[symbolDispose]();
        captureTable4.delete(handleEntry.rep);
      } else if (TerminalInput[symbolCabiDispose]) {
        TerminalInput[symbolCabiDispose](handleEntry.rep);
      }
    }
  }
  function trampoline15(handle) {
    const handleEntry = rscTableRemove(handleTable5, handle);
    if (handleEntry.own) {
      
      const rsc = captureTable5.get(handleEntry.rep);
      if (rsc) {
        if (rsc[symbolDispose]) rsc[symbolDispose]();
        captureTable5.delete(handleEntry.rep);
      } else if (TerminalOutput[symbolCabiDispose]) {
        TerminalOutput[symbolCabiDispose](handleEntry.rep);
      }
    }
  }
  Promise.all([module0, module1, module2, module3, module4]).catch(() => {});
  ({ exports: exports0 } = yield instantiateCore(yield module2));
  ({ exports: exports1 } = yield instantiateCore(yield module0, {
    '[export]sqlite:wasm/high-level@0.1.0': {
      '[resource-new]connection': trampoline0,
      '[resource-new]statement': trampoline1,
    },
    'wasi:io/poll@0.2.0': {
      '[resource-drop]pollable': trampoline2,
    },
    'wasi:io/streams@0.2.0': {
      '[resource-drop]input-stream': trampoline3,
      '[resource-drop]output-stream': trampoline4,
    },
    'wasi:sockets/tcp@0.2.0': {
      '[resource-drop]tcp-socket': trampoline8,
    },
    'wasi:sockets/udp@0.2.0': {
      '[resource-drop]incoming-datagram-stream': trampoline6,
      '[resource-drop]outgoing-datagram-stream': trampoline7,
      '[resource-drop]udp-socket': trampoline5,
    },
    wasi_snapshot_preview1: {
      adapter_close_badfd: exports0['15'],
      clock_time_get: exports0['0'],
      fd_close: exports0['1'],
      fd_fdstat_get: exports0['2'],
      fd_filestat_get: exports0['3'],
      fd_filestat_set_size: exports0['4'],
      fd_prestat_dir_name: exports0['6'],
      fd_prestat_get: exports0['5'],
      fd_read: exports0['7'],
      fd_seek: exports0['8'],
      fd_sync: exports0['9'],
      fd_write: exports0['10'],
      path_filestat_get: exports0['11'],
      path_open: exports0['12'],
      path_unlink_file: exports0['13'],
      proc_exit: exports0['14'],
    },
  }));
  ({ exports: exports2 } = yield instantiateCore(yield module1, {
    __main_module__: {
      cabi_realloc: exports1.cabi_realloc,
    },
    env: {
      memory: exports1.memory,
    },
    'wasi:cli/exit@0.2.0': {
      exit: trampoline18,
    },
    'wasi:cli/stderr@0.2.0': {
      'get-stderr': trampoline13,
    },
    'wasi:cli/stdin@0.2.0': {
      'get-stdin': trampoline16,
    },
    'wasi:cli/stdout@0.2.0': {
      'get-stdout': trampoline17,
    },
    'wasi:cli/terminal-input@0.2.0': {
      '[resource-drop]terminal-input': trampoline14,
    },
    'wasi:cli/terminal-output@0.2.0': {
      '[resource-drop]terminal-output': trampoline15,
    },
    'wasi:cli/terminal-stderr@0.2.0': {
      'get-terminal-stderr': exports0['42'],
    },
    'wasi:cli/terminal-stdin@0.2.0': {
      'get-terminal-stdin': exports0['40'],
    },
    'wasi:cli/terminal-stdout@0.2.0': {
      'get-terminal-stdout': exports0['41'],
    },
    'wasi:clocks/monotonic-clock@0.2.0': {
      now: trampoline9,
    },
    'wasi:clocks/wall-clock@0.2.0': {
      now: exports0['18'],
    },
    'wasi:filesystem/preopens@0.2.0': {
      'get-directories': exports0['39'],
    },
    'wasi:filesystem/types@0.2.0': {
      '[method]descriptor.append-via-stream': exports0['28'],
      '[method]descriptor.get-flags': exports0['19'],
      '[method]descriptor.get-type': exports0['29'],
      '[method]descriptor.metadata-hash': exports0['31'],
      '[method]descriptor.metadata-hash-at': exports0['32'],
      '[method]descriptor.open-at': exports0['24'],
      '[method]descriptor.read-via-stream': exports0['26'],
      '[method]descriptor.set-size': exports0['20'],
      '[method]descriptor.stat': exports0['30'],
      '[method]descriptor.stat-at': exports0['23'],
      '[method]descriptor.sync': exports0['22'],
      '[method]descriptor.unlink-file-at': exports0['25'],
      '[method]descriptor.write-via-stream': exports0['27'],
      '[resource-drop]descriptor': trampoline11,
      '[resource-drop]directory-entry-stream': trampoline10,
      'filesystem-error-code': exports0['21'],
    },
    'wasi:io/error@0.2.0': {
      '[resource-drop]error': trampoline12,
    },
    'wasi:io/streams@0.2.0': {
      '[method]input-stream.blocking-read': exports0['34'],
      '[method]input-stream.read': exports0['33'],
      '[method]output-stream.blocking-flush': exports0['37'],
      '[method]output-stream.blocking-write-and-flush': exports0['38'],
      '[method]output-stream.check-write': exports0['35'],
      '[method]output-stream.write': exports0['36'],
      '[resource-drop]input-stream': trampoline3,
      '[resource-drop]output-stream': trampoline4,
    },
  }));
  memory0 = exports1.memory;
  realloc0 = exports2.cabi_import_realloc;
  ({ exports: exports3 } = yield instantiateCore(yield module3, {
    '': {
      $imports: exports0.$imports,
      '0': exports2.clock_time_get,
      '1': exports2.fd_close,
      '10': exports2.fd_write,
      '11': exports2.path_filestat_get,
      '12': exports2.path_open,
      '13': exports2.path_unlink_file,
      '14': exports2.proc_exit,
      '15': exports2.adapter_close_badfd,
      '16': exports1['sqlite:wasm/high-level@0.1.0#[dtor]connection'],
      '17': exports1['sqlite:wasm/high-level@0.1.0#[dtor]statement'],
      '18': trampoline19,
      '19': trampoline20,
      '2': exports2.fd_fdstat_get,
      '20': trampoline21,
      '21': trampoline22,
      '22': trampoline23,
      '23': trampoline24,
      '24': trampoline25,
      '25': trampoline26,
      '26': trampoline27,
      '27': trampoline28,
      '28': trampoline29,
      '29': trampoline30,
      '3': exports2.fd_filestat_get,
      '30': trampoline31,
      '31': trampoline32,
      '32': trampoline33,
      '33': trampoline34,
      '34': trampoline35,
      '35': trampoline36,
      '36': trampoline37,
      '37': trampoline38,
      '38': trampoline39,
      '39': trampoline40,
      '4': exports2.fd_filestat_set_size,
      '40': trampoline41,
      '41': trampoline42,
      '42': trampoline43,
      '5': exports2.fd_prestat_get,
      '6': exports2.fd_prestat_dir_name,
      '7': exports2.fd_read,
      '8': exports2.fd_seek,
      '9': exports2.fd_sync,
    },
  }));
  ({ exports: exports4 } = yield instantiateCore(yield module4, {
    '': {
      '': exports1._initialize,
    },
  }));
  realloc1 = exports1.cabi_realloc;
  postReturn0 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#exec'];
  postReturn1 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#column-name'];
  postReturn2 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#column-text'];
  postReturn3 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#column-blob'];
  postReturn4 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#errmsg'];
  postReturn5 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#libversion'];
  postReturn6 = exports1['cabi_post_sqlite:wasm/low-level@0.1.0#sourceid'];
  postReturn7 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.execute'];
  postReturn8 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.execute-with-params'];
  postReturn9 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.query'];
  postReturn10 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.query-with-params'];
  postReturn11 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.prepare'];
  postReturn12 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.begin-transaction'];
  postReturn13 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.commit'];
  postReturn14 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.rollback'];
  postReturn15 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]connection.last-error'];
  postReturn16 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.bind'];
  postReturn17 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.bind-all'];
  postReturn18 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.execute'];
  postReturn19 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.query'];
  postReturn20 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.step'];
  postReturn21 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.reset'];
  postReturn22 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.clear-bindings'];
  postReturn23 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#[method]statement.column-names'];
  postReturn24 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#version'];
  postReturn25 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#open-memory'];
  postReturn26 = exports1['cabi_post_sqlite:wasm/high-level@0.1.0#open-file'];
  let lowLevel010Open;
  
  function open(arg0, arg1) {
    var ptr0 = utf8Encode(arg0, realloc1, memory0);
    var len0 = utf8EncodedLen;
    let flags1 = 0;
    if (typeof arg1 === 'object' && arg1 !== null) {
      flags1 = Boolean(arg1.readonly) << 0 | Boolean(arg1.readwrite) << 1 | Boolean(arg1.create) << 2 | Boolean(arg1.memory) << 3 | Boolean(arg1.uri) << 4;
    } else if (arg1 !== null && arg1!== undefined) {
      throw new TypeError('only an object, undefined or null can be converted to flags');
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="open"][Instruction::CallWasm] enter', {
      funcName: 'open',
      paramCount: 3,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Open');
    const ret = lowLevel010Open(ptr0, len0, flags1);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: BigInt.asUintN(64, dataView(memory0).getBigInt64(ret + 8, true))
        };
        break;
      }
      case 1: {
        let enum2;
        switch (dataView(memory0).getUint8(ret + 8, true)) {
          case 0: {
            enum2 = 'ok';
            break;
          }
          case 1: {
            enum2 = 'error';
            break;
          }
          case 2: {
            enum2 = 'internal';
            break;
          }
          case 3: {
            enum2 = 'perm';
            break;
          }
          case 4: {
            enum2 = 'abort';
            break;
          }
          case 5: {
            enum2 = 'busy';
            break;
          }
          case 6: {
            enum2 = 'locked';
            break;
          }
          case 7: {
            enum2 = 'nomem';
            break;
          }
          case 8: {
            enum2 = 'readonly';
            break;
          }
          case 9: {
            enum2 = 'interrupt';
            break;
          }
          case 10: {
            enum2 = 'ioerr';
            break;
          }
          case 11: {
            enum2 = 'corrupt';
            break;
          }
          case 12: {
            enum2 = 'notfound';
            break;
          }
          case 13: {
            enum2 = 'full';
            break;
          }
          case 14: {
            enum2 = 'cantopen';
            break;
          }
          case 15: {
            enum2 = 'protocol';
            break;
          }
          case 16: {
            enum2 = 'empty';
            break;
          }
          case 17: {
            enum2 = 'schema';
            break;
          }
          case 18: {
            enum2 = 'toobig';
            break;
          }
          case 19: {
            enum2 = 'constraint';
            break;
          }
          case 20: {
            enum2 = 'mismatch';
            break;
          }
          case 21: {
            enum2 = 'misuse';
            break;
          }
          case 22: {
            enum2 = 'nolfs';
            break;
          }
          case 23: {
            enum2 = 'auth';
            break;
          }
          case 24: {
            enum2 = 'format';
            break;
          }
          case 25: {
            enum2 = 'range';
            break;
          }
          case 26: {
            enum2 = 'notadb';
            break;
          }
          case 27: {
            enum2 = 'notice';
            break;
          }
          case 28: {
            enum2 = 'warning';
            break;
          }
          case 29: {
            enum2 = 'row';
            break;
          }
          case 30: {
            enum2 = 'done';
            break;
          }
          default: {
            throw new TypeError('invalid discriminant specified for ResultCode');
          }
        }
        variant3= {
          tag: 'err',
          val: enum2
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="open"][Instruction::Return]', {
      funcName: 'open',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    const retCopy = variant3;
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  }
  let lowLevel010Close;
  
  function close(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="close"][Instruction::CallWasm] enter', {
      funcName: 'close',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Close');
    const ret = lowLevel010Close(toUint64(arg0));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="close"][Instruction::Return]', {
      funcName: 'close',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010Exec;
  
  function exec(arg0, arg1) {
    var ptr0 = utf8Encode(arg1, realloc1, memory0);
    var len0 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="exec"][Instruction::CallWasm] enter', {
      funcName: 'exec',
      paramCount: 3,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Exec');
    const ret = lowLevel010Exec(toUint64(arg0), ptr0, len0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var ptr1 = dataView(memory0).getUint32(ret + 4, true);
        var len1 = dataView(memory0).getUint32(ret + 8, true);
        var result1 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr1, len1));
        variant3= {
          tag: 'ok',
          val: result1
        };
        break;
      }
      case 1: {
        let enum2;
        switch (dataView(memory0).getUint8(ret + 4, true)) {
          case 0: {
            enum2 = 'ok';
            break;
          }
          case 1: {
            enum2 = 'error';
            break;
          }
          case 2: {
            enum2 = 'internal';
            break;
          }
          case 3: {
            enum2 = 'perm';
            break;
          }
          case 4: {
            enum2 = 'abort';
            break;
          }
          case 5: {
            enum2 = 'busy';
            break;
          }
          case 6: {
            enum2 = 'locked';
            break;
          }
          case 7: {
            enum2 = 'nomem';
            break;
          }
          case 8: {
            enum2 = 'readonly';
            break;
          }
          case 9: {
            enum2 = 'interrupt';
            break;
          }
          case 10: {
            enum2 = 'ioerr';
            break;
          }
          case 11: {
            enum2 = 'corrupt';
            break;
          }
          case 12: {
            enum2 = 'notfound';
            break;
          }
          case 13: {
            enum2 = 'full';
            break;
          }
          case 14: {
            enum2 = 'cantopen';
            break;
          }
          case 15: {
            enum2 = 'protocol';
            break;
          }
          case 16: {
            enum2 = 'empty';
            break;
          }
          case 17: {
            enum2 = 'schema';
            break;
          }
          case 18: {
            enum2 = 'toobig';
            break;
          }
          case 19: {
            enum2 = 'constraint';
            break;
          }
          case 20: {
            enum2 = 'mismatch';
            break;
          }
          case 21: {
            enum2 = 'misuse';
            break;
          }
          case 22: {
            enum2 = 'nolfs';
            break;
          }
          case 23: {
            enum2 = 'auth';
            break;
          }
          case 24: {
            enum2 = 'format';
            break;
          }
          case 25: {
            enum2 = 'range';
            break;
          }
          case 26: {
            enum2 = 'notadb';
            break;
          }
          case 27: {
            enum2 = 'notice';
            break;
          }
          case 28: {
            enum2 = 'warning';
            break;
          }
          case 29: {
            enum2 = 'row';
            break;
          }
          case 30: {
            enum2 = 'done';
            break;
          }
          default: {
            throw new TypeError('invalid discriminant specified for ResultCode');
          }
        }
        variant3= {
          tag: 'err',
          val: enum2
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="exec"][Instruction::Return]', {
      funcName: 'exec',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn0(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  }
  let lowLevel010Prepare;
  
  function prepare(arg0, arg1) {
    var ptr0 = utf8Encode(arg1, realloc1, memory0);
    var len0 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="prepare"][Instruction::CallWasm] enter', {
      funcName: 'prepare',
      paramCount: 3,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Prepare');
    const ret = lowLevel010Prepare(toUint64(arg0), ptr0, len0);
    endCurrentTask(0);
    let variant2;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant2= {
          tag: 'ok',
          val: BigInt.asUintN(64, dataView(memory0).getBigInt64(ret + 8, true))
        };
        break;
      }
      case 1: {
        let enum1;
        switch (dataView(memory0).getUint8(ret + 8, true)) {
          case 0: {
            enum1 = 'ok';
            break;
          }
          case 1: {
            enum1 = 'error';
            break;
          }
          case 2: {
            enum1 = 'internal';
            break;
          }
          case 3: {
            enum1 = 'perm';
            break;
          }
          case 4: {
            enum1 = 'abort';
            break;
          }
          case 5: {
            enum1 = 'busy';
            break;
          }
          case 6: {
            enum1 = 'locked';
            break;
          }
          case 7: {
            enum1 = 'nomem';
            break;
          }
          case 8: {
            enum1 = 'readonly';
            break;
          }
          case 9: {
            enum1 = 'interrupt';
            break;
          }
          case 10: {
            enum1 = 'ioerr';
            break;
          }
          case 11: {
            enum1 = 'corrupt';
            break;
          }
          case 12: {
            enum1 = 'notfound';
            break;
          }
          case 13: {
            enum1 = 'full';
            break;
          }
          case 14: {
            enum1 = 'cantopen';
            break;
          }
          case 15: {
            enum1 = 'protocol';
            break;
          }
          case 16: {
            enum1 = 'empty';
            break;
          }
          case 17: {
            enum1 = 'schema';
            break;
          }
          case 18: {
            enum1 = 'toobig';
            break;
          }
          case 19: {
            enum1 = 'constraint';
            break;
          }
          case 20: {
            enum1 = 'mismatch';
            break;
          }
          case 21: {
            enum1 = 'misuse';
            break;
          }
          case 22: {
            enum1 = 'nolfs';
            break;
          }
          case 23: {
            enum1 = 'auth';
            break;
          }
          case 24: {
            enum1 = 'format';
            break;
          }
          case 25: {
            enum1 = 'range';
            break;
          }
          case 26: {
            enum1 = 'notadb';
            break;
          }
          case 27: {
            enum1 = 'notice';
            break;
          }
          case 28: {
            enum1 = 'warning';
            break;
          }
          case 29: {
            enum1 = 'row';
            break;
          }
          case 30: {
            enum1 = 'done';
            break;
          }
          default: {
            throw new TypeError('invalid discriminant specified for ResultCode');
          }
        }
        variant2= {
          tag: 'err',
          val: enum1
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="prepare"][Instruction::Return]', {
      funcName: 'prepare',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    const retCopy = variant2;
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  }
  let lowLevel010Step;
  
  function step(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="step"][Instruction::CallWasm] enter', {
      funcName: 'step',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Step');
    const ret = lowLevel010Step(toUint64(arg0));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="step"][Instruction::Return]', {
      funcName: 'step',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010Reset;
  
  function reset(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="reset"][Instruction::CallWasm] enter', {
      funcName: 'reset',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Reset');
    const ret = lowLevel010Reset(toUint64(arg0));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="reset"][Instruction::Return]', {
      funcName: 'reset',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010Finalize;
  
  function finalize(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="finalize"][Instruction::CallWasm] enter', {
      funcName: 'finalize',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Finalize');
    const ret = lowLevel010Finalize(toUint64(arg0));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="finalize"][Instruction::Return]', {
      funcName: 'finalize',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010BindNull;
  
  function bindNull(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-null"][Instruction::CallWasm] enter', {
      funcName: 'bind-null',
      paramCount: 2,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindNull');
    const ret = lowLevel010BindNull(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-null"][Instruction::Return]', {
      funcName: 'bind-null',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010BindInt;
  
  function bindInt(arg0, arg1, arg2) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-int"][Instruction::CallWasm] enter', {
      funcName: 'bind-int',
      paramCount: 3,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindInt');
    const ret = lowLevel010BindInt(toUint64(arg0), toInt32(arg1), toInt32(arg2));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-int"][Instruction::Return]', {
      funcName: 'bind-int',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010BindInt64;
  
  function bindInt64(arg0, arg1, arg2) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-int64"][Instruction::CallWasm] enter', {
      funcName: 'bind-int64',
      paramCount: 3,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindInt64');
    const ret = lowLevel010BindInt64(toUint64(arg0), toInt32(arg1), toInt64(arg2));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-int64"][Instruction::Return]', {
      funcName: 'bind-int64',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010BindDouble;
  
  function bindDouble(arg0, arg1, arg2) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-double"][Instruction::CallWasm] enter', {
      funcName: 'bind-double',
      paramCount: 3,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindDouble');
    const ret = lowLevel010BindDouble(toUint64(arg0), toInt32(arg1), +arg2);
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-double"][Instruction::Return]', {
      funcName: 'bind-double',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010BindText;
  
  function bindText(arg0, arg1, arg2) {
    var ptr0 = utf8Encode(arg2, realloc1, memory0);
    var len0 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-text"][Instruction::CallWasm] enter', {
      funcName: 'bind-text',
      paramCount: 4,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindText');
    const ret = lowLevel010BindText(toUint64(arg0), toInt32(arg1), ptr0, len0);
    endCurrentTask(0);
    let enum1;
    switch (ret) {
      case 0: {
        enum1 = 'ok';
        break;
      }
      case 1: {
        enum1 = 'error';
        break;
      }
      case 2: {
        enum1 = 'internal';
        break;
      }
      case 3: {
        enum1 = 'perm';
        break;
      }
      case 4: {
        enum1 = 'abort';
        break;
      }
      case 5: {
        enum1 = 'busy';
        break;
      }
      case 6: {
        enum1 = 'locked';
        break;
      }
      case 7: {
        enum1 = 'nomem';
        break;
      }
      case 8: {
        enum1 = 'readonly';
        break;
      }
      case 9: {
        enum1 = 'interrupt';
        break;
      }
      case 10: {
        enum1 = 'ioerr';
        break;
      }
      case 11: {
        enum1 = 'corrupt';
        break;
      }
      case 12: {
        enum1 = 'notfound';
        break;
      }
      case 13: {
        enum1 = 'full';
        break;
      }
      case 14: {
        enum1 = 'cantopen';
        break;
      }
      case 15: {
        enum1 = 'protocol';
        break;
      }
      case 16: {
        enum1 = 'empty';
        break;
      }
      case 17: {
        enum1 = 'schema';
        break;
      }
      case 18: {
        enum1 = 'toobig';
        break;
      }
      case 19: {
        enum1 = 'constraint';
        break;
      }
      case 20: {
        enum1 = 'mismatch';
        break;
      }
      case 21: {
        enum1 = 'misuse';
        break;
      }
      case 22: {
        enum1 = 'nolfs';
        break;
      }
      case 23: {
        enum1 = 'auth';
        break;
      }
      case 24: {
        enum1 = 'format';
        break;
      }
      case 25: {
        enum1 = 'range';
        break;
      }
      case 26: {
        enum1 = 'notadb';
        break;
      }
      case 27: {
        enum1 = 'notice';
        break;
      }
      case 28: {
        enum1 = 'warning';
        break;
      }
      case 29: {
        enum1 = 'row';
        break;
      }
      case 30: {
        enum1 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-text"][Instruction::Return]', {
      funcName: 'bind-text',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum1;
  }
  let lowLevel010BindBlob;
  
  function bindBlob(arg0, arg1, arg2) {
    var val0 = arg2;
    var len0 = val0.byteLength;
    var ptr0 = realloc1(0, 0, 1, len0 * 1);
    var src0 = new Uint8Array(val0.buffer || val0, val0.byteOffset, len0 * 1);
    (new Uint8Array(memory0.buffer, ptr0, len0 * 1)).set(src0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-blob"][Instruction::CallWasm] enter', {
      funcName: 'bind-blob',
      paramCount: 4,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindBlob');
    const ret = lowLevel010BindBlob(toUint64(arg0), toInt32(arg1), ptr0, len0);
    endCurrentTask(0);
    let enum1;
    switch (ret) {
      case 0: {
        enum1 = 'ok';
        break;
      }
      case 1: {
        enum1 = 'error';
        break;
      }
      case 2: {
        enum1 = 'internal';
        break;
      }
      case 3: {
        enum1 = 'perm';
        break;
      }
      case 4: {
        enum1 = 'abort';
        break;
      }
      case 5: {
        enum1 = 'busy';
        break;
      }
      case 6: {
        enum1 = 'locked';
        break;
      }
      case 7: {
        enum1 = 'nomem';
        break;
      }
      case 8: {
        enum1 = 'readonly';
        break;
      }
      case 9: {
        enum1 = 'interrupt';
        break;
      }
      case 10: {
        enum1 = 'ioerr';
        break;
      }
      case 11: {
        enum1 = 'corrupt';
        break;
      }
      case 12: {
        enum1 = 'notfound';
        break;
      }
      case 13: {
        enum1 = 'full';
        break;
      }
      case 14: {
        enum1 = 'cantopen';
        break;
      }
      case 15: {
        enum1 = 'protocol';
        break;
      }
      case 16: {
        enum1 = 'empty';
        break;
      }
      case 17: {
        enum1 = 'schema';
        break;
      }
      case 18: {
        enum1 = 'toobig';
        break;
      }
      case 19: {
        enum1 = 'constraint';
        break;
      }
      case 20: {
        enum1 = 'mismatch';
        break;
      }
      case 21: {
        enum1 = 'misuse';
        break;
      }
      case 22: {
        enum1 = 'nolfs';
        break;
      }
      case 23: {
        enum1 = 'auth';
        break;
      }
      case 24: {
        enum1 = 'format';
        break;
      }
      case 25: {
        enum1 = 'range';
        break;
      }
      case 26: {
        enum1 = 'notadb';
        break;
      }
      case 27: {
        enum1 = 'notice';
        break;
      }
      case 28: {
        enum1 = 'warning';
        break;
      }
      case 29: {
        enum1 = 'row';
        break;
      }
      case 30: {
        enum1 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-blob"][Instruction::Return]', {
      funcName: 'bind-blob',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum1;
  }
  let lowLevel010BindParameterCount;
  
  function bindParameterCount(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-parameter-count"][Instruction::CallWasm] enter', {
      funcName: 'bind-parameter-count',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindParameterCount');
    const ret = lowLevel010BindParameterCount(toUint64(arg0));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-parameter-count"][Instruction::Return]', {
      funcName: 'bind-parameter-count',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010BindParameterIndex;
  
  function bindParameterIndex(arg0, arg1) {
    var ptr0 = utf8Encode(arg1, realloc1, memory0);
    var len0 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-parameter-index"][Instruction::CallWasm] enter', {
      funcName: 'bind-parameter-index',
      paramCount: 3,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010BindParameterIndex');
    const ret = lowLevel010BindParameterIndex(toUint64(arg0), ptr0, len0);
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="bind-parameter-index"][Instruction::Return]', {
      funcName: 'bind-parameter-index',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010ClearBindings;
  
  function clearBindings(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="clear-bindings"][Instruction::CallWasm] enter', {
      funcName: 'clear-bindings',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ClearBindings');
    const ret = lowLevel010ClearBindings(toUint64(arg0));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="clear-bindings"][Instruction::Return]', {
      funcName: 'clear-bindings',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010ColumnCount;
  
  function columnCount(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-count"][Instruction::CallWasm] enter', {
      funcName: 'column-count',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnCount');
    const ret = lowLevel010ColumnCount(toUint64(arg0));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-count"][Instruction::Return]', {
      funcName: 'column-count',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010ColumnName;
  
  function columnName(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-name"][Instruction::CallWasm] enter', {
      funcName: 'column-name',
      paramCount: 2,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnName');
    const ret = lowLevel010ColumnName(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr0, len0));
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-name"][Instruction::Return]', {
      funcName: 'column-name',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn1(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let lowLevel010GetColumnType;
  
  function getColumnType(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="get-column-type"][Instruction::CallWasm] enter', {
      funcName: 'get-column-type',
      paramCount: 2,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010GetColumnType');
    const ret = lowLevel010GetColumnType(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'integer';
        break;
      }
      case 1: {
        enum0 = 'float';
        break;
      }
      case 2: {
        enum0 = 'text';
        break;
      }
      case 3: {
        enum0 = 'blob';
        break;
      }
      case 4: {
        enum0 = 'null';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ColumnType');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="get-column-type"][Instruction::Return]', {
      funcName: 'get-column-type',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010ColumnInt;
  
  function columnInt(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-int"][Instruction::CallWasm] enter', {
      funcName: 'column-int',
      paramCount: 2,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnInt');
    const ret = lowLevel010ColumnInt(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-int"][Instruction::Return]', {
      funcName: 'column-int',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010ColumnInt64;
  
  function columnInt64(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-int64"][Instruction::CallWasm] enter', {
      funcName: 'column-int64',
      paramCount: 2,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnInt64');
    const ret = lowLevel010ColumnInt64(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-int64"][Instruction::Return]', {
      funcName: 'column-int64',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010ColumnDouble;
  
  function columnDouble(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-double"][Instruction::CallWasm] enter', {
      funcName: 'column-double',
      paramCount: 2,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnDouble');
    const ret = lowLevel010ColumnDouble(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-double"][Instruction::Return]', {
      funcName: 'column-double',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010ColumnText;
  
  function columnText(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-text"][Instruction::CallWasm] enter', {
      funcName: 'column-text',
      paramCount: 2,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnText');
    const ret = lowLevel010ColumnText(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr0, len0));
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-text"][Instruction::Return]', {
      funcName: 'column-text',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn2(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let lowLevel010ColumnBlob;
  
  function columnBlob(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-blob"][Instruction::CallWasm] enter', {
      funcName: 'column-blob',
      paramCount: 2,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnBlob');
    const ret = lowLevel010ColumnBlob(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = new Uint8Array(memory0.buffer.slice(ptr0, ptr0 + len0 * 1));
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-blob"][Instruction::Return]', {
      funcName: 'column-blob',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn3(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let lowLevel010ColumnBytes;
  
  function columnBytes(arg0, arg1) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-bytes"][Instruction::CallWasm] enter', {
      funcName: 'column-bytes',
      paramCount: 2,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ColumnBytes');
    const ret = lowLevel010ColumnBytes(toUint64(arg0), toInt32(arg1));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="column-bytes"][Instruction::Return]', {
      funcName: 'column-bytes',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010Errmsg;
  
  function errmsg(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="errmsg"][Instruction::CallWasm] enter', {
      funcName: 'errmsg',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Errmsg');
    const ret = lowLevel010Errmsg(toUint64(arg0));
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr0, len0));
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="errmsg"][Instruction::Return]', {
      funcName: 'errmsg',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn4(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let lowLevel010Errcode;
  
  function errcode(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="errcode"][Instruction::CallWasm] enter', {
      funcName: 'errcode',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Errcode');
    const ret = lowLevel010Errcode(toUint64(arg0));
    endCurrentTask(0);
    let enum0;
    switch (ret) {
      case 0: {
        enum0 = 'ok';
        break;
      }
      case 1: {
        enum0 = 'error';
        break;
      }
      case 2: {
        enum0 = 'internal';
        break;
      }
      case 3: {
        enum0 = 'perm';
        break;
      }
      case 4: {
        enum0 = 'abort';
        break;
      }
      case 5: {
        enum0 = 'busy';
        break;
      }
      case 6: {
        enum0 = 'locked';
        break;
      }
      case 7: {
        enum0 = 'nomem';
        break;
      }
      case 8: {
        enum0 = 'readonly';
        break;
      }
      case 9: {
        enum0 = 'interrupt';
        break;
      }
      case 10: {
        enum0 = 'ioerr';
        break;
      }
      case 11: {
        enum0 = 'corrupt';
        break;
      }
      case 12: {
        enum0 = 'notfound';
        break;
      }
      case 13: {
        enum0 = 'full';
        break;
      }
      case 14: {
        enum0 = 'cantopen';
        break;
      }
      case 15: {
        enum0 = 'protocol';
        break;
      }
      case 16: {
        enum0 = 'empty';
        break;
      }
      case 17: {
        enum0 = 'schema';
        break;
      }
      case 18: {
        enum0 = 'toobig';
        break;
      }
      case 19: {
        enum0 = 'constraint';
        break;
      }
      case 20: {
        enum0 = 'mismatch';
        break;
      }
      case 21: {
        enum0 = 'misuse';
        break;
      }
      case 22: {
        enum0 = 'nolfs';
        break;
      }
      case 23: {
        enum0 = 'auth';
        break;
      }
      case 24: {
        enum0 = 'format';
        break;
      }
      case 25: {
        enum0 = 'range';
        break;
      }
      case 26: {
        enum0 = 'notadb';
        break;
      }
      case 27: {
        enum0 = 'notice';
        break;
      }
      case 28: {
        enum0 = 'warning';
        break;
      }
      case 29: {
        enum0 = 'row';
        break;
      }
      case 30: {
        enum0 = 'done';
        break;
      }
      default: {
        throw new TypeError('invalid discriminant specified for ResultCode');
      }
    }
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="errcode"][Instruction::Return]', {
      funcName: 'errcode',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return enum0;
  }
  let lowLevel010ExtendedErrcode;
  
  function extendedErrcode(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="extended-errcode"][Instruction::CallWasm] enter', {
      funcName: 'extended-errcode',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010ExtendedErrcode');
    const ret = lowLevel010ExtendedErrcode(toUint64(arg0));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="extended-errcode"][Instruction::Return]', {
      funcName: 'extended-errcode',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010GetAutocommit;
  
  function getAutocommit(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="get-autocommit"][Instruction::CallWasm] enter', {
      funcName: 'get-autocommit',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010GetAutocommit');
    const ret = lowLevel010GetAutocommit(toUint64(arg0));
    endCurrentTask(0);
    var bool0 = ret;
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="get-autocommit"][Instruction::Return]', {
      funcName: 'get-autocommit',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return bool0 == 0 ? false : (bool0 == 1 ? true : throwInvalidBool());
  }
  let lowLevel010Changes;
  
  function changes(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="changes"][Instruction::CallWasm] enter', {
      funcName: 'changes',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Changes');
    const ret = lowLevel010Changes(toUint64(arg0));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="changes"][Instruction::Return]', {
      funcName: 'changes',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010TotalChanges;
  
  function totalChanges(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="total-changes"][Instruction::CallWasm] enter', {
      funcName: 'total-changes',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010TotalChanges');
    const ret = lowLevel010TotalChanges(toUint64(arg0));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="total-changes"][Instruction::Return]', {
      funcName: 'total-changes',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010LastInsertRowid;
  
  function lastInsertRowid(arg0) {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="last-insert-rowid"][Instruction::CallWasm] enter', {
      funcName: 'last-insert-rowid',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010LastInsertRowid');
    const ret = lowLevel010LastInsertRowid(toUint64(arg0));
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="last-insert-rowid"][Instruction::Return]', {
      funcName: 'last-insert-rowid',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010Libversion;
  
  function libversion() {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="libversion"][Instruction::CallWasm] enter', {
      funcName: 'libversion',
      paramCount: 0,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Libversion');
    const ret = lowLevel010Libversion();
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr0, len0));
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="libversion"][Instruction::Return]', {
      funcName: 'libversion',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn5(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let lowLevel010LibversionNumber;
  
  function libversionNumber() {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="libversion-number"][Instruction::CallWasm] enter', {
      funcName: 'libversion-number',
      paramCount: 0,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010LibversionNumber');
    const ret = lowLevel010LibversionNumber();
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="libversion-number"][Instruction::Return]', {
      funcName: 'libversion-number',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let lowLevel010Sourceid;
  
  function sourceid() {
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="sourceid"][Instruction::CallWasm] enter', {
      funcName: 'sourceid',
      paramCount: 0,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'lowLevel010Sourceid');
    const ret = lowLevel010Sourceid();
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr0, len0));
    _debugLog('[iface="sqlite:wasm/low-level@0.1.0", function="sourceid"][Instruction::Return]', {
      funcName: 'sourceid',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn6(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let highLevel010ConstructorConnection;
  
  class Connection{
    constructor(arg0, arg1) {
      var ptr0 = utf8Encode(arg0, realloc1, memory0);
      var len0 = utf8EncodedLen;
      var val1 = arg1;
      let enum1;
      switch (val1) {
        case 'read-only': {
          enum1 = 0;
          break;
        }
        case 'read-write': {
          enum1 = 1;
          break;
        }
        case 'read-write-create': {
          enum1 = 2;
          break;
        }
        case 'memory': {
          enum1 = 3;
          break;
        }
        default: {
          if ((arg1) instanceof Error) {
            console.error(arg1);
          }
          
          throw new TypeError(`"${val1}" is not one of the cases of open-mode`);
        }
      }
      _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[constructor]connection"][Instruction::CallWasm] enter', {
        funcName: '[constructor]connection',
        paramCount: 3,
        async: false,
        postReturn: false,
      });
      const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010ConstructorConnection');
      const ret = highLevel010ConstructorConnection(ptr0, len0, enum1);
      endCurrentTask(0);
      var handle3 = ret;
      var rsc2 = new.target === Connection ? this : Object.create(Connection.prototype);
      Object.defineProperty(rsc2, symbolRscHandle, { writable: true, value: handle3});
      finalizationRegistry12.register(rsc2, handle3, rsc2);
      Object.defineProperty(rsc2, symbolDispose, { writable: true, value: function () {
        finalizationRegistry12.unregister(rsc2);
        rscTableRemove(handleTable12, handle3);
        rsc2[symbolDispose] = emptyFunc;
        rsc2[symbolRscHandle] = undefined;
        exports0['16'](handleTable12[(handle3 << 1) + 1] & ~T_FLAG);
      }});
      _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[constructor]connection"][Instruction::Return]', {
        funcName: '[constructor]connection',
        paramCount: 1,
        async: false,
        postReturn: false
      });
      return rsc2;
    }
  }
  let highLevel010MethodConnectionExecute;
  
  Connection.prototype.execute = function execute(arg1) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    var ptr2 = utf8Encode(arg1, realloc1, memory0);
    var len2 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.execute"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.execute',
      paramCount: 3,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionExecute');
    const ret = highLevel010MethodConnectionExecute(handle0, ptr2, len2);
    endCurrentTask(0);
    let variant4;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant4= {
          tag: 'ok',
          val: {
            changes: dataView(memory0).getInt32(ret + 8, true),
            lastInsertRowid: dataView(memory0).getBigInt64(ret + 16, true),
          }
        };
        break;
      }
      case 1: {
        var ptr3 = dataView(memory0).getUint32(ret + 16, true);
        var len3 = dataView(memory0).getUint32(ret + 20, true);
        var result3 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr3, len3));
        variant4= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 8, true),
            extendedCode: dataView(memory0).getInt32(ret + 12, true),
            message: result3,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.execute"][Instruction::Return]', {
      funcName: '[method]connection.execute',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant4;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn7(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionExecuteWithParams;
  
  Connection.prototype.executeWithParams = function executeWithParams(arg1, arg2) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    var ptr2 = utf8Encode(arg1, realloc1, memory0);
    var len2 = utf8EncodedLen;
    var vec6 = arg2;
    var len6 = vec6.length;
    var result6 = realloc1(0, 0, 8, len6 * 16);
    for (let i = 0; i < vec6.length; i++) {
      const e = vec6[i];
      const base = result6 + i * 16;var variant5 = e;
      switch (variant5.tag) {
        case 'null': {
          dataView(memory0).setInt8(base + 0, 0, true);
          break;
        }
        case 'integer': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 1, true);
          dataView(memory0).setBigInt64(base + 8, toInt64(e), true);
          break;
        }
        case 'real': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 2, true);
          dataView(memory0).setFloat64(base + 8, +e, true);
          break;
        }
        case 'text': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 3, true);
          var ptr3 = utf8Encode(e, realloc1, memory0);
          var len3 = utf8EncodedLen;
          dataView(memory0).setUint32(base + 12, len3, true);
          dataView(memory0).setUint32(base + 8, ptr3, true);
          break;
        }
        case 'blob': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 4, true);
          var val4 = e;
          var len4 = val4.byteLength;
          var ptr4 = realloc1(0, 0, 1, len4 * 1);
          var src4 = new Uint8Array(val4.buffer || val4, val4.byteOffset, len4 * 1);
          (new Uint8Array(memory0.buffer, ptr4, len4 * 1)).set(src4);
          dataView(memory0).setUint32(base + 12, len4, true);
          dataView(memory0).setUint32(base + 8, ptr4, true);
          break;
        }
        default: {
          throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant5.tag)}\` (received \`${variant5}\`) specified for \`Value\``);
        }
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.execute-with-params"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.execute-with-params',
      paramCount: 5,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionExecuteWithParams');
    const ret = highLevel010MethodConnectionExecuteWithParams(handle0, ptr2, len2, result6, len6);
    endCurrentTask(0);
    let variant8;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant8= {
          tag: 'ok',
          val: {
            changes: dataView(memory0).getInt32(ret + 8, true),
            lastInsertRowid: dataView(memory0).getBigInt64(ret + 16, true),
          }
        };
        break;
      }
      case 1: {
        var ptr7 = dataView(memory0).getUint32(ret + 16, true);
        var len7 = dataView(memory0).getUint32(ret + 20, true);
        var result7 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr7, len7));
        variant8= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 8, true),
            extendedCode: dataView(memory0).getInt32(ret + 12, true),
            message: result7,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.execute-with-params"][Instruction::Return]', {
      funcName: '[method]connection.execute-with-params',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant8;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn8(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionQuery;
  
  Connection.prototype.query = function query(arg1) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    var ptr2 = utf8Encode(arg1, realloc1, memory0);
    var len2 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.query"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.query',
      paramCount: 3,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionQuery');
    const ret = highLevel010MethodConnectionQuery(handle0, ptr2, len2);
    endCurrentTask(0);
    let variant11;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var len4 = dataView(memory0).getUint32(ret + 8, true);
        var base4 = dataView(memory0).getUint32(ret + 4, true);
        var result4 = [];
        for (let i = 0; i < len4; i++) {
          const base = base4 + i * 8;
          var ptr3 = dataView(memory0).getUint32(base + 0, true);
          var len3 = dataView(memory0).getUint32(base + 4, true);
          var result3 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr3, len3));
          result4.push(result3);
        }
        var len9 = dataView(memory0).getUint32(ret + 16, true);
        var base9 = dataView(memory0).getUint32(ret + 12, true);
        var result9 = [];
        for (let i = 0; i < len9; i++) {
          const base = base9 + i * 8;
          var len8 = dataView(memory0).getUint32(base + 4, true);
          var base8 = dataView(memory0).getUint32(base + 0, true);
          var result8 = [];
          for (let i = 0; i < len8; i++) {
            const base = base8 + i * 16;
            let variant7;
            switch (dataView(memory0).getUint8(base + 0, true)) {
              case 0: {
                variant7= {
                  tag: 'null',
                };
                break;
              }
              case 1: {
                variant7= {
                  tag: 'integer',
                  val: dataView(memory0).getBigInt64(base + 8, true)
                };
                break;
              }
              case 2: {
                variant7= {
                  tag: 'real',
                  val: dataView(memory0).getFloat64(base + 8, true)
                };
                break;
              }
              case 3: {
                var ptr5 = dataView(memory0).getUint32(base + 8, true);
                var len5 = dataView(memory0).getUint32(base + 12, true);
                var result5 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr5, len5));
                variant7= {
                  tag: 'text',
                  val: result5
                };
                break;
              }
              case 4: {
                var ptr6 = dataView(memory0).getUint32(base + 8, true);
                var len6 = dataView(memory0).getUint32(base + 12, true);
                var result6 = new Uint8Array(memory0.buffer.slice(ptr6, ptr6 + len6 * 1));
                variant7= {
                  tag: 'blob',
                  val: result6
                };
                break;
              }
              default: {
                throw new TypeError('invalid variant discriminant for Value');
              }
            }
            result8.push(variant7);
          }
          result9.push({
            columns: result8,
          });
        }
        variant11= {
          tag: 'ok',
          val: {
            columnNames: result4,
            rows: result9,
          }
        };
        break;
      }
      case 1: {
        var ptr10 = dataView(memory0).getUint32(ret + 12, true);
        var len10 = dataView(memory0).getUint32(ret + 16, true);
        var result10 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr10, len10));
        variant11= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result10,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.query"][Instruction::Return]', {
      funcName: '[method]connection.query',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant11;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn9(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionQueryWithParams;
  
  Connection.prototype.queryWithParams = function queryWithParams(arg1, arg2) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    var ptr2 = utf8Encode(arg1, realloc1, memory0);
    var len2 = utf8EncodedLen;
    var vec6 = arg2;
    var len6 = vec6.length;
    var result6 = realloc1(0, 0, 8, len6 * 16);
    for (let i = 0; i < vec6.length; i++) {
      const e = vec6[i];
      const base = result6 + i * 16;var variant5 = e;
      switch (variant5.tag) {
        case 'null': {
          dataView(memory0).setInt8(base + 0, 0, true);
          break;
        }
        case 'integer': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 1, true);
          dataView(memory0).setBigInt64(base + 8, toInt64(e), true);
          break;
        }
        case 'real': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 2, true);
          dataView(memory0).setFloat64(base + 8, +e, true);
          break;
        }
        case 'text': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 3, true);
          var ptr3 = utf8Encode(e, realloc1, memory0);
          var len3 = utf8EncodedLen;
          dataView(memory0).setUint32(base + 12, len3, true);
          dataView(memory0).setUint32(base + 8, ptr3, true);
          break;
        }
        case 'blob': {
          const e = variant5.val;
          dataView(memory0).setInt8(base + 0, 4, true);
          var val4 = e;
          var len4 = val4.byteLength;
          var ptr4 = realloc1(0, 0, 1, len4 * 1);
          var src4 = new Uint8Array(val4.buffer || val4, val4.byteOffset, len4 * 1);
          (new Uint8Array(memory0.buffer, ptr4, len4 * 1)).set(src4);
          dataView(memory0).setUint32(base + 12, len4, true);
          dataView(memory0).setUint32(base + 8, ptr4, true);
          break;
        }
        default: {
          throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant5.tag)}\` (received \`${variant5}\`) specified for \`Value\``);
        }
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.query-with-params"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.query-with-params',
      paramCount: 5,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionQueryWithParams');
    const ret = highLevel010MethodConnectionQueryWithParams(handle0, ptr2, len2, result6, len6);
    endCurrentTask(0);
    let variant15;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var len8 = dataView(memory0).getUint32(ret + 8, true);
        var base8 = dataView(memory0).getUint32(ret + 4, true);
        var result8 = [];
        for (let i = 0; i < len8; i++) {
          const base = base8 + i * 8;
          var ptr7 = dataView(memory0).getUint32(base + 0, true);
          var len7 = dataView(memory0).getUint32(base + 4, true);
          var result7 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr7, len7));
          result8.push(result7);
        }
        var len13 = dataView(memory0).getUint32(ret + 16, true);
        var base13 = dataView(memory0).getUint32(ret + 12, true);
        var result13 = [];
        for (let i = 0; i < len13; i++) {
          const base = base13 + i * 8;
          var len12 = dataView(memory0).getUint32(base + 4, true);
          var base12 = dataView(memory0).getUint32(base + 0, true);
          var result12 = [];
          for (let i = 0; i < len12; i++) {
            const base = base12 + i * 16;
            let variant11;
            switch (dataView(memory0).getUint8(base + 0, true)) {
              case 0: {
                variant11= {
                  tag: 'null',
                };
                break;
              }
              case 1: {
                variant11= {
                  tag: 'integer',
                  val: dataView(memory0).getBigInt64(base + 8, true)
                };
                break;
              }
              case 2: {
                variant11= {
                  tag: 'real',
                  val: dataView(memory0).getFloat64(base + 8, true)
                };
                break;
              }
              case 3: {
                var ptr9 = dataView(memory0).getUint32(base + 8, true);
                var len9 = dataView(memory0).getUint32(base + 12, true);
                var result9 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr9, len9));
                variant11= {
                  tag: 'text',
                  val: result9
                };
                break;
              }
              case 4: {
                var ptr10 = dataView(memory0).getUint32(base + 8, true);
                var len10 = dataView(memory0).getUint32(base + 12, true);
                var result10 = new Uint8Array(memory0.buffer.slice(ptr10, ptr10 + len10 * 1));
                variant11= {
                  tag: 'blob',
                  val: result10
                };
                break;
              }
              default: {
                throw new TypeError('invalid variant discriminant for Value');
              }
            }
            result12.push(variant11);
          }
          result13.push({
            columns: result12,
          });
        }
        variant15= {
          tag: 'ok',
          val: {
            columnNames: result8,
            rows: result13,
          }
        };
        break;
      }
      case 1: {
        var ptr14 = dataView(memory0).getUint32(ret + 12, true);
        var len14 = dataView(memory0).getUint32(ret + 16, true);
        var result14 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr14, len14));
        variant15= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result14,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.query-with-params"][Instruction::Return]', {
      funcName: '[method]connection.query-with-params',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant15;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn10(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionPrepare;
  
  Connection.prototype.prepare = function prepare(arg1) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    var ptr2 = utf8Encode(arg1, realloc1, memory0);
    var len2 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.prepare"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.prepare',
      paramCount: 3,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionPrepare');
    const ret = highLevel010MethodConnectionPrepare(handle0, ptr2, len2);
    endCurrentTask(0);
    let variant6;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var handle4 = dataView(memory0).getInt32(ret + 4, true);
        var rsc3 = new.target === Statement ? this : Object.create(Statement.prototype);
        Object.defineProperty(rsc3, symbolRscHandle, { writable: true, value: handle4});
        finalizationRegistry13.register(rsc3, handle4, rsc3);
        Object.defineProperty(rsc3, symbolDispose, { writable: true, value: function () {
          finalizationRegistry13.unregister(rsc3);
          rscTableRemove(handleTable13, handle4);
          rsc3[symbolDispose] = emptyFunc;
          rsc3[symbolRscHandle] = undefined;
          exports0['17'](handleTable13[(handle4 << 1) + 1] & ~T_FLAG);
        }});
        variant6= {
          tag: 'ok',
          val: rsc3
        };
        break;
      }
      case 1: {
        var ptr5 = dataView(memory0).getUint32(ret + 12, true);
        var len5 = dataView(memory0).getUint32(ret + 16, true);
        var result5 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr5, len5));
        variant6= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result5,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.prepare"][Instruction::Return]', {
      funcName: '[method]connection.prepare',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant6;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn11(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionBeginTransaction;
  
  Connection.prototype.beginTransaction = function beginTransaction() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.begin-transaction"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.begin-transaction',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionBeginTransaction');
    const ret = highLevel010MethodConnectionBeginTransaction(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.begin-transaction"][Instruction::Return]', {
      funcName: '[method]connection.begin-transaction',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn12(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionCommit;
  
  Connection.prototype.commit = function commit() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.commit"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.commit',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionCommit');
    const ret = highLevel010MethodConnectionCommit(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.commit"][Instruction::Return]', {
      funcName: '[method]connection.commit',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn13(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionRollback;
  
  Connection.prototype.rollback = function rollback() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.rollback"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.rollback',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionRollback');
    const ret = highLevel010MethodConnectionRollback(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.rollback"][Instruction::Return]', {
      funcName: '[method]connection.rollback',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn14(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodConnectionInAutocommit;
  
  Connection.prototype.inAutocommit = function inAutocommit() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.in-autocommit"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.in-autocommit',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionInAutocommit');
    const ret = highLevel010MethodConnectionInAutocommit(handle0);
    endCurrentTask(0);
    var bool2 = ret;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.in-autocommit"][Instruction::Return]', {
      funcName: '[method]connection.in-autocommit',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return bool2 == 0 ? false : (bool2 == 1 ? true : throwInvalidBool());
  };
  let highLevel010MethodConnectionLastError;
  
  Connection.prototype.lastError = function lastError() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable12[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Connection" resource.');
    }
    var handle0 = handleTable12[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.last-error"][Instruction::CallWasm] enter', {
      funcName: '[method]connection.last-error',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodConnectionLastError');
    const ret = highLevel010MethodConnectionLastError(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3 = undefined;
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3 = {
          code: dataView(memory0).getInt32(ret + 4, true),
          extendedCode: dataView(memory0).getInt32(ret + 8, true),
          message: result2,
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for option');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]connection.last-error"][Instruction::Return]', {
      funcName: '[method]connection.last-error',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn15(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  };
  let highLevel010MethodStatementBind;
  
  class Statement{
    constructor () {
      throw new Error('"Statement" resource does not define a constructor');
    }
  }
  
  Statement.prototype.bind = function bind(arg1, arg2) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    var variant4 = arg2;
    let variant4_0;
    let variant4_1;
    let variant4_2;
    switch (variant4.tag) {
      case 'null': {
        variant4_0 = 0;
        variant4_1 = 0n;
        variant4_2 = 0;
        break;
      }
      case 'integer': {
        const e = variant4.val;
        variant4_0 = 1;
        variant4_1 = BigInt(toInt64(e));
        variant4_2 = 0;
        break;
      }
      case 'real': {
        const e = variant4.val;
        variant4_0 = 2;
        variant4_1 = BigInt(f64ToI64(+e));
        variant4_2 = 0;
        break;
      }
      case 'text': {
        const e = variant4.val;
        var ptr2 = utf8Encode(e, realloc1, memory0);
        var len2 = utf8EncodedLen;
        variant4_0 = 3;
        variant4_1 = BigInt(ptr2);
        variant4_2 = len2;
        break;
      }
      case 'blob': {
        const e = variant4.val;
        var val3 = e;
        var len3 = val3.byteLength;
        var ptr3 = realloc1(0, 0, 1, len3 * 1);
        var src3 = new Uint8Array(val3.buffer || val3, val3.byteOffset, len3 * 1);
        (new Uint8Array(memory0.buffer, ptr3, len3 * 1)).set(src3);
        variant4_0 = 4;
        variant4_1 = BigInt(ptr3);
        variant4_2 = len3;
        break;
      }
      default: {
        throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant4.tag)}\` (received \`${variant4}\`) specified for \`Value\``);
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.bind"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.bind',
      paramCount: 5,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementBind');
    const ret = highLevel010MethodStatementBind(handle0, toInt32(arg1), variant4_0, variant4_1, variant4_2);
    endCurrentTask(0);
    let variant6;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant6= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr5 = dataView(memory0).getUint32(ret + 12, true);
        var len5 = dataView(memory0).getUint32(ret + 16, true);
        var result5 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr5, len5));
        variant6= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result5,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.bind"][Instruction::Return]', {
      funcName: '[method]statement.bind',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant6;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn16(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementBindAll;
  
  Statement.prototype.bindAll = function bindAll(arg1) {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    var vec5 = arg1;
    var len5 = vec5.length;
    var result5 = realloc1(0, 0, 8, len5 * 16);
    for (let i = 0; i < vec5.length; i++) {
      const e = vec5[i];
      const base = result5 + i * 16;var variant4 = e;
      switch (variant4.tag) {
        case 'null': {
          dataView(memory0).setInt8(base + 0, 0, true);
          break;
        }
        case 'integer': {
          const e = variant4.val;
          dataView(memory0).setInt8(base + 0, 1, true);
          dataView(memory0).setBigInt64(base + 8, toInt64(e), true);
          break;
        }
        case 'real': {
          const e = variant4.val;
          dataView(memory0).setInt8(base + 0, 2, true);
          dataView(memory0).setFloat64(base + 8, +e, true);
          break;
        }
        case 'text': {
          const e = variant4.val;
          dataView(memory0).setInt8(base + 0, 3, true);
          var ptr2 = utf8Encode(e, realloc1, memory0);
          var len2 = utf8EncodedLen;
          dataView(memory0).setUint32(base + 12, len2, true);
          dataView(memory0).setUint32(base + 8, ptr2, true);
          break;
        }
        case 'blob': {
          const e = variant4.val;
          dataView(memory0).setInt8(base + 0, 4, true);
          var val3 = e;
          var len3 = val3.byteLength;
          var ptr3 = realloc1(0, 0, 1, len3 * 1);
          var src3 = new Uint8Array(val3.buffer || val3, val3.byteOffset, len3 * 1);
          (new Uint8Array(memory0.buffer, ptr3, len3 * 1)).set(src3);
          dataView(memory0).setUint32(base + 12, len3, true);
          dataView(memory0).setUint32(base + 8, ptr3, true);
          break;
        }
        default: {
          throw new TypeError(`invalid variant tag value \`${JSON.stringify(variant4.tag)}\` (received \`${variant4}\`) specified for \`Value\``);
        }
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.bind-all"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.bind-all',
      paramCount: 3,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementBindAll');
    const ret = highLevel010MethodStatementBindAll(handle0, result5, len5);
    endCurrentTask(0);
    let variant7;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant7= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr6 = dataView(memory0).getUint32(ret + 12, true);
        var len6 = dataView(memory0).getUint32(ret + 16, true);
        var result6 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr6, len6));
        variant7= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result6,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.bind-all"][Instruction::Return]', {
      funcName: '[method]statement.bind-all',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant7;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn17(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementExecute;
  
  Statement.prototype.execute = function execute() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.execute"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.execute',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementExecute');
    const ret = highLevel010MethodStatementExecute(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: {
            changes: dataView(memory0).getInt32(ret + 8, true),
            lastInsertRowid: dataView(memory0).getBigInt64(ret + 16, true),
          }
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 16, true);
        var len2 = dataView(memory0).getUint32(ret + 20, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 8, true),
            extendedCode: dataView(memory0).getInt32(ret + 12, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.execute"][Instruction::Return]', {
      funcName: '[method]statement.execute',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn18(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementQuery;
  
  Statement.prototype.query = function query() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.query"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.query',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementQuery');
    const ret = highLevel010MethodStatementQuery(handle0);
    endCurrentTask(0);
    let variant10;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var len3 = dataView(memory0).getUint32(ret + 8, true);
        var base3 = dataView(memory0).getUint32(ret + 4, true);
        var result3 = [];
        for (let i = 0; i < len3; i++) {
          const base = base3 + i * 8;
          var ptr2 = dataView(memory0).getUint32(base + 0, true);
          var len2 = dataView(memory0).getUint32(base + 4, true);
          var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
          result3.push(result2);
        }
        var len8 = dataView(memory0).getUint32(ret + 16, true);
        var base8 = dataView(memory0).getUint32(ret + 12, true);
        var result8 = [];
        for (let i = 0; i < len8; i++) {
          const base = base8 + i * 8;
          var len7 = dataView(memory0).getUint32(base + 4, true);
          var base7 = dataView(memory0).getUint32(base + 0, true);
          var result7 = [];
          for (let i = 0; i < len7; i++) {
            const base = base7 + i * 16;
            let variant6;
            switch (dataView(memory0).getUint8(base + 0, true)) {
              case 0: {
                variant6= {
                  tag: 'null',
                };
                break;
              }
              case 1: {
                variant6= {
                  tag: 'integer',
                  val: dataView(memory0).getBigInt64(base + 8, true)
                };
                break;
              }
              case 2: {
                variant6= {
                  tag: 'real',
                  val: dataView(memory0).getFloat64(base + 8, true)
                };
                break;
              }
              case 3: {
                var ptr4 = dataView(memory0).getUint32(base + 8, true);
                var len4 = dataView(memory0).getUint32(base + 12, true);
                var result4 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr4, len4));
                variant6= {
                  tag: 'text',
                  val: result4
                };
                break;
              }
              case 4: {
                var ptr5 = dataView(memory0).getUint32(base + 8, true);
                var len5 = dataView(memory0).getUint32(base + 12, true);
                var result5 = new Uint8Array(memory0.buffer.slice(ptr5, ptr5 + len5 * 1));
                variant6= {
                  tag: 'blob',
                  val: result5
                };
                break;
              }
              default: {
                throw new TypeError('invalid variant discriminant for Value');
              }
            }
            result7.push(variant6);
          }
          result8.push({
            columns: result7,
          });
        }
        variant10= {
          tag: 'ok',
          val: {
            columnNames: result3,
            rows: result8,
          }
        };
        break;
      }
      case 1: {
        var ptr9 = dataView(memory0).getUint32(ret + 12, true);
        var len9 = dataView(memory0).getUint32(ret + 16, true);
        var result9 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr9, len9));
        variant10= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result9,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.query"][Instruction::Return]', {
      funcName: '[method]statement.query',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant10;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn19(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementStep;
  
  Statement.prototype.step = function step() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.step"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.step',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementStep');
    const ret = highLevel010MethodStatementStep(handle0);
    endCurrentTask(0);
    let variant8;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        let variant6;
        switch (dataView(memory0).getUint8(ret + 4, true)) {
          case 0: {
            variant6 = undefined;
            break;
          }
          case 1: {
            var len5 = dataView(memory0).getUint32(ret + 12, true);
            var base5 = dataView(memory0).getUint32(ret + 8, true);
            var result5 = [];
            for (let i = 0; i < len5; i++) {
              const base = base5 + i * 16;
              let variant4;
              switch (dataView(memory0).getUint8(base + 0, true)) {
                case 0: {
                  variant4= {
                    tag: 'null',
                  };
                  break;
                }
                case 1: {
                  variant4= {
                    tag: 'integer',
                    val: dataView(memory0).getBigInt64(base + 8, true)
                  };
                  break;
                }
                case 2: {
                  variant4= {
                    tag: 'real',
                    val: dataView(memory0).getFloat64(base + 8, true)
                  };
                  break;
                }
                case 3: {
                  var ptr2 = dataView(memory0).getUint32(base + 8, true);
                  var len2 = dataView(memory0).getUint32(base + 12, true);
                  var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
                  variant4= {
                    tag: 'text',
                    val: result2
                  };
                  break;
                }
                case 4: {
                  var ptr3 = dataView(memory0).getUint32(base + 8, true);
                  var len3 = dataView(memory0).getUint32(base + 12, true);
                  var result3 = new Uint8Array(memory0.buffer.slice(ptr3, ptr3 + len3 * 1));
                  variant4= {
                    tag: 'blob',
                    val: result3
                  };
                  break;
                }
                default: {
                  throw new TypeError('invalid variant discriminant for Value');
                }
              }
              result5.push(variant4);
            }
            variant6 = {
              columns: result5,
            };
            break;
          }
          default: {
            throw new TypeError('invalid variant discriminant for option');
          }
        }
        variant8= {
          tag: 'ok',
          val: variant6
        };
        break;
      }
      case 1: {
        var ptr7 = dataView(memory0).getUint32(ret + 12, true);
        var len7 = dataView(memory0).getUint32(ret + 16, true);
        var result7 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr7, len7));
        variant8= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result7,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.step"][Instruction::Return]', {
      funcName: '[method]statement.step',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant8;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn20(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementReset;
  
  Statement.prototype.reset = function reset() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.reset"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.reset',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementReset');
    const ret = highLevel010MethodStatementReset(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.reset"][Instruction::Return]', {
      funcName: '[method]statement.reset',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn21(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementClearBindings;
  
  Statement.prototype.clearBindings = function clearBindings() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.clear-bindings"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.clear-bindings',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementClearBindings');
    const ret = highLevel010MethodStatementClearBindings(handle0);
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        variant3= {
          tag: 'ok',
          val: undefined
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.clear-bindings"][Instruction::Return]', {
      funcName: '[method]statement.clear-bindings',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn22(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  };
  let highLevel010MethodStatementColumnCount;
  
  Statement.prototype.columnCount = function columnCount() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.column-count"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.column-count',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementColumnCount');
    const ret = highLevel010MethodStatementColumnCount(handle0);
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.column-count"][Instruction::Return]', {
      funcName: '[method]statement.column-count',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  };
  let highLevel010MethodStatementColumnNames;
  
  Statement.prototype.columnNames = function columnNames() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.column-names"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.column-names',
      paramCount: 1,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementColumnNames');
    const ret = highLevel010MethodStatementColumnNames(handle0);
    endCurrentTask(0);
    var len3 = dataView(memory0).getUint32(ret + 4, true);
    var base3 = dataView(memory0).getUint32(ret + 0, true);
    var result3 = [];
    for (let i = 0; i < len3; i++) {
      const base = base3 + i * 8;
      var ptr2 = dataView(memory0).getUint32(base + 0, true);
      var len2 = dataView(memory0).getUint32(base + 4, true);
      var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
      result3.push(result2);
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.column-names"][Instruction::Return]', {
      funcName: '[method]statement.column-names',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn23(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  };
  let highLevel010MethodStatementParameterCount;
  
  Statement.prototype.parameterCount = function parameterCount() {
    var handle1 = this[symbolRscHandle];
    if (!handle1 || (handleTable13[(handle1 << 1) + 1] & T_FLAG) === 0) {
      throw new TypeError('Resource error: Not a valid "Statement" resource.');
    }
    var handle0 = handleTable13[(handle1 << 1) + 1] & ~T_FLAG;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.parameter-count"][Instruction::CallWasm] enter', {
      funcName: '[method]statement.parameter-count',
      paramCount: 1,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010MethodStatementParameterCount');
    const ret = highLevel010MethodStatementParameterCount(handle0);
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="[method]statement.parameter-count"][Instruction::Return]', {
      funcName: '[method]statement.parameter-count',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  };
  let highLevel010Version;
  
  function version() {
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="version"][Instruction::CallWasm] enter', {
      funcName: 'version',
      paramCount: 0,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010Version');
    const ret = highLevel010Version();
    endCurrentTask(0);
    var ptr0 = dataView(memory0).getUint32(ret + 0, true);
    var len0 = dataView(memory0).getUint32(ret + 4, true);
    var result0 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr0, len0));
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="version"][Instruction::Return]', {
      funcName: 'version',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = result0;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn24(ret);
    cstate.mayLeave = true;
    return retCopy;
    
  }
  let highLevel010VersionNumber;
  
  function versionNumber() {
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="version-number"][Instruction::CallWasm] enter', {
      funcName: 'version-number',
      paramCount: 0,
      async: false,
      postReturn: false,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010VersionNumber');
    const ret = highLevel010VersionNumber();
    endCurrentTask(0);
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="version-number"][Instruction::Return]', {
      funcName: 'version-number',
      paramCount: 1,
      async: false,
      postReturn: false
    });
    return ret;
  }
  let highLevel010OpenMemory;
  
  function openMemory() {
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="open-memory"][Instruction::CallWasm] enter', {
      funcName: 'open-memory',
      paramCount: 0,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010OpenMemory');
    const ret = highLevel010OpenMemory();
    endCurrentTask(0);
    let variant3;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var handle1 = dataView(memory0).getInt32(ret + 4, true);
        var rsc0 = new.target === Connection ? this : Object.create(Connection.prototype);
        Object.defineProperty(rsc0, symbolRscHandle, { writable: true, value: handle1});
        finalizationRegistry12.register(rsc0, handle1, rsc0);
        Object.defineProperty(rsc0, symbolDispose, { writable: true, value: function () {
          finalizationRegistry12.unregister(rsc0);
          rscTableRemove(handleTable12, handle1);
          rsc0[symbolDispose] = emptyFunc;
          rsc0[symbolRscHandle] = undefined;
          exports0['16'](handleTable12[(handle1 << 1) + 1] & ~T_FLAG);
        }});
        variant3= {
          tag: 'ok',
          val: rsc0
        };
        break;
      }
      case 1: {
        var ptr2 = dataView(memory0).getUint32(ret + 12, true);
        var len2 = dataView(memory0).getUint32(ret + 16, true);
        var result2 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr2, len2));
        variant3= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result2,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="open-memory"][Instruction::Return]', {
      funcName: 'open-memory',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant3;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn25(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  }
  let highLevel010OpenFile;
  
  function openFile(arg0) {
    var ptr0 = utf8Encode(arg0, realloc1, memory0);
    var len0 = utf8EncodedLen;
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="open-file"][Instruction::CallWasm] enter', {
      funcName: 'open-file',
      paramCount: 2,
      async: false,
      postReturn: true,
    });
    const _wasm_call_currentTaskID = startCurrentTask(0, false, 'highLevel010OpenFile');
    const ret = highLevel010OpenFile(ptr0, len0);
    endCurrentTask(0);
    let variant4;
    switch (dataView(memory0).getUint8(ret + 0, true)) {
      case 0: {
        var handle2 = dataView(memory0).getInt32(ret + 4, true);
        var rsc1 = new.target === Connection ? this : Object.create(Connection.prototype);
        Object.defineProperty(rsc1, symbolRscHandle, { writable: true, value: handle2});
        finalizationRegistry12.register(rsc1, handle2, rsc1);
        Object.defineProperty(rsc1, symbolDispose, { writable: true, value: function () {
          finalizationRegistry12.unregister(rsc1);
          rscTableRemove(handleTable12, handle2);
          rsc1[symbolDispose] = emptyFunc;
          rsc1[symbolRscHandle] = undefined;
          exports0['16'](handleTable12[(handle2 << 1) + 1] & ~T_FLAG);
        }});
        variant4= {
          tag: 'ok',
          val: rsc1
        };
        break;
      }
      case 1: {
        var ptr3 = dataView(memory0).getUint32(ret + 12, true);
        var len3 = dataView(memory0).getUint32(ret + 16, true);
        var result3 = utf8Decoder.decode(new Uint8Array(memory0.buffer, ptr3, len3));
        variant4= {
          tag: 'err',
          val: {
            code: dataView(memory0).getInt32(ret + 4, true),
            extendedCode: dataView(memory0).getInt32(ret + 8, true),
            message: result3,
          }
        };
        break;
      }
      default: {
        throw new TypeError('invalid variant discriminant for expected');
      }
    }
    _debugLog('[iface="sqlite:wasm/high-level@0.1.0", function="open-file"][Instruction::Return]', {
      funcName: 'open-file',
      paramCount: 1,
      async: false,
      postReturn: true
    });
    const retCopy = variant4;
    
    let cstate = getOrCreateAsyncState(0);
    cstate.mayLeave = false;
    postReturn26(ret);
    cstate.mayLeave = true;
    
    
    
    if (typeof retCopy === 'object' && retCopy.tag === 'err') {
      throw new ComponentError(retCopy.val);
    }
    return retCopy.val;
    
  }
  lowLevel010Open = exports1['sqlite:wasm/low-level@0.1.0#open'];
  lowLevel010Close = exports1['sqlite:wasm/low-level@0.1.0#close'];
  lowLevel010Exec = exports1['sqlite:wasm/low-level@0.1.0#exec'];
  lowLevel010Prepare = exports1['sqlite:wasm/low-level@0.1.0#prepare'];
  lowLevel010Step = exports1['sqlite:wasm/low-level@0.1.0#step'];
  lowLevel010Reset = exports1['sqlite:wasm/low-level@0.1.0#reset'];
  lowLevel010Finalize = exports1['sqlite:wasm/low-level@0.1.0#finalize'];
  lowLevel010BindNull = exports1['sqlite:wasm/low-level@0.1.0#bind-null'];
  lowLevel010BindInt = exports1['sqlite:wasm/low-level@0.1.0#bind-int'];
  lowLevel010BindInt64 = exports1['sqlite:wasm/low-level@0.1.0#bind-int64'];
  lowLevel010BindDouble = exports1['sqlite:wasm/low-level@0.1.0#bind-double'];
  lowLevel010BindText = exports1['sqlite:wasm/low-level@0.1.0#bind-text'];
  lowLevel010BindBlob = exports1['sqlite:wasm/low-level@0.1.0#bind-blob'];
  lowLevel010BindParameterCount = exports1['sqlite:wasm/low-level@0.1.0#bind-parameter-count'];
  lowLevel010BindParameterIndex = exports1['sqlite:wasm/low-level@0.1.0#bind-parameter-index'];
  lowLevel010ClearBindings = exports1['sqlite:wasm/low-level@0.1.0#clear-bindings'];
  lowLevel010ColumnCount = exports1['sqlite:wasm/low-level@0.1.0#column-count'];
  lowLevel010ColumnName = exports1['sqlite:wasm/low-level@0.1.0#column-name'];
  lowLevel010GetColumnType = exports1['sqlite:wasm/low-level@0.1.0#get-column-type'];
  lowLevel010ColumnInt = exports1['sqlite:wasm/low-level@0.1.0#column-int'];
  lowLevel010ColumnInt64 = exports1['sqlite:wasm/low-level@0.1.0#column-int64'];
  lowLevel010ColumnDouble = exports1['sqlite:wasm/low-level@0.1.0#column-double'];
  lowLevel010ColumnText = exports1['sqlite:wasm/low-level@0.1.0#column-text'];
  lowLevel010ColumnBlob = exports1['sqlite:wasm/low-level@0.1.0#column-blob'];
  lowLevel010ColumnBytes = exports1['sqlite:wasm/low-level@0.1.0#column-bytes'];
  lowLevel010Errmsg = exports1['sqlite:wasm/low-level@0.1.0#errmsg'];
  lowLevel010Errcode = exports1['sqlite:wasm/low-level@0.1.0#errcode'];
  lowLevel010ExtendedErrcode = exports1['sqlite:wasm/low-level@0.1.0#extended-errcode'];
  lowLevel010GetAutocommit = exports1['sqlite:wasm/low-level@0.1.0#get-autocommit'];
  lowLevel010Changes = exports1['sqlite:wasm/low-level@0.1.0#changes'];
  lowLevel010TotalChanges = exports1['sqlite:wasm/low-level@0.1.0#total-changes'];
  lowLevel010LastInsertRowid = exports1['sqlite:wasm/low-level@0.1.0#last-insert-rowid'];
  lowLevel010Libversion = exports1['sqlite:wasm/low-level@0.1.0#libversion'];
  lowLevel010LibversionNumber = exports1['sqlite:wasm/low-level@0.1.0#libversion-number'];
  lowLevel010Sourceid = exports1['sqlite:wasm/low-level@0.1.0#sourceid'];
  highLevel010ConstructorConnection = exports1['sqlite:wasm/high-level@0.1.0#[constructor]connection'];
  highLevel010MethodConnectionExecute = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.execute'];
  highLevel010MethodConnectionExecuteWithParams = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.execute-with-params'];
  highLevel010MethodConnectionQuery = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.query'];
  highLevel010MethodConnectionQueryWithParams = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.query-with-params'];
  highLevel010MethodConnectionPrepare = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.prepare'];
  highLevel010MethodConnectionBeginTransaction = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.begin-transaction'];
  highLevel010MethodConnectionCommit = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.commit'];
  highLevel010MethodConnectionRollback = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.rollback'];
  highLevel010MethodConnectionInAutocommit = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.in-autocommit'];
  highLevel010MethodConnectionLastError = exports1['sqlite:wasm/high-level@0.1.0#[method]connection.last-error'];
  highLevel010MethodStatementBind = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.bind'];
  highLevel010MethodStatementBindAll = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.bind-all'];
  highLevel010MethodStatementExecute = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.execute'];
  highLevel010MethodStatementQuery = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.query'];
  highLevel010MethodStatementStep = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.step'];
  highLevel010MethodStatementReset = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.reset'];
  highLevel010MethodStatementClearBindings = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.clear-bindings'];
  highLevel010MethodStatementColumnCount = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.column-count'];
  highLevel010MethodStatementColumnNames = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.column-names'];
  highLevel010MethodStatementParameterCount = exports1['sqlite:wasm/high-level@0.1.0#[method]statement.parameter-count'];
  highLevel010Version = exports1['sqlite:wasm/high-level@0.1.0#version'];
  highLevel010VersionNumber = exports1['sqlite:wasm/high-level@0.1.0#version-number'];
  highLevel010OpenMemory = exports1['sqlite:wasm/high-level@0.1.0#open-memory'];
  highLevel010OpenFile = exports1['sqlite:wasm/high-level@0.1.0#open-file'];
  const highLevel010 = {
    Connection: Connection,
    Statement: Statement,
    openFile: openFile,
    openMemory: openMemory,
    version: version,
    versionNumber: versionNumber,
    
  };
  const lowLevel010 = {
    bindBlob: bindBlob,
    bindDouble: bindDouble,
    bindInt: bindInt,
    bindInt64: bindInt64,
    bindNull: bindNull,
    bindParameterCount: bindParameterCount,
    bindParameterIndex: bindParameterIndex,
    bindText: bindText,
    changes: changes,
    clearBindings: clearBindings,
    close: close,
    columnBlob: columnBlob,
    columnBytes: columnBytes,
    columnCount: columnCount,
    columnDouble: columnDouble,
    columnInt: columnInt,
    columnInt64: columnInt64,
    columnName: columnName,
    columnText: columnText,
    errcode: errcode,
    errmsg: errmsg,
    exec: exec,
    extendedErrcode: extendedErrcode,
    finalize: finalize,
    getAutocommit: getAutocommit,
    getColumnType: getColumnType,
    lastInsertRowid: lastInsertRowid,
    libversion: libversion,
    libversionNumber: libversionNumber,
    open: open,
    prepare: prepare,
    reset: reset,
    sourceid: sourceid,
    step: step,
    totalChanges: totalChanges,
    
  };
  
  return { highLevel: highLevel010, lowLevel: lowLevel010, 'sqlite:wasm/high-level@0.1.0': highLevel010, 'sqlite:wasm/low-level@0.1.0': lowLevel010,  };
})();
let promise, resolve, reject;
function runNext (value) {
  try {
    let done;
    do {
      ({ value, done } = gen.next(value));
    } while (!(value instanceof Promise) && !done);
    if (done) {
      if (resolve) return resolve(value);
      else return value;
    }
    if (!promise) promise = new Promise((_resolve, _reject) => (resolve = _resolve, reject = _reject));
    value.then(nextVal => done ? resolve() : runNext(nextVal), reject);
  }
  catch (e) {
    if (reject) reject(e);
    else throw e;
  }
}
const maybeSyncReturn = runNext(null);
return promise || maybeSyncReturn;
}
