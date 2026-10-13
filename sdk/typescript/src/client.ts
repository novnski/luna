// The TypeScript LUNA client (CORE-05). Its operation table, types and JSON
// Schemas are generated from the Rust SDK; this runtime validates every
// request and reply against those schemas and follows the same contract:
// stable command ids, exact targets, fences, a lost reply read back by
// receipt (never resent under a new id), and watches with cursor repair.

import { MANIFEST } from './generated.ts';
import type {
  ApiDescription,
  CommandReceipt,
  EngineInfo,
  ErrorKind,
  ExecuteTaskCommandParams,
  JsonSchema,
  OperationEntry,
  OperationName,
  Operations,
  TaskCommandRead,
  TaskCommandSnapshot,
  TaskCommandUpdate,
  TaskCommandWatchItem,
  TaskIntent,
} from './generated.ts';
import { validate } from './validate.ts';

/** The same kinds and messages as the Rust `SdkError`. */
export class LunaSdkError extends Error {
  readonly kind: ErrorKind;
  readonly operation: string | undefined;
  /** For `unknownOutcome`: read this command's receipt; never resend under a new id. */
  readonly commandId: string | undefined;

  constructor(kind: ErrorKind, message: string, detail: { operation?: string; commandId?: string; cause?: unknown } = {}) {
    super(message, detail.cause === undefined ? undefined : { cause: detail.cause });
    this.name = 'LunaSdkError';
    this.kind = kind;
    this.operation = detail.operation;
    this.commandId = detail.commandId;
  }
}

/** Who issues a command. Every receipt records these three values. */
export interface Actor {
  actorId: string;
  clientId: string;
  clientSessionId: string;
}

/** An actor with a fresh client session id. */
export function newActor(actorId: string, clientId: string): Actor {
  return { actorId, clientId, clientSessionId: crypto.randomUUID() };
}

export interface TaskCommandInput {
  actor: Actor;
  taskId: string;
  intent: TaskIntent;
  /** Stable idempotency key; minted once when omitted. */
  commandId?: string;
  /** The task's execution host when it is not the connected engine. Never inferred. */
  targetDeviceId?: string;
  /** Exact reviewed fences; without it the task's current snapshot is read. */
  reviewed?: TaskCommandSnapshot;
  issuedAtMs?: number;
}

export interface ConnectOptions {
  /** Local owner IPC port (`ws://127.0.0.1:{port}`). */
  port?: number;
  url?: string;
  /** Bound every call; omit to wait for each reply. */
  timeoutMs?: number;
}

const OWNER_IPC = 'luna-owner-ipc-v1';
/** Matches the Rust client: a stalled consumer loses its stream instead of growing memory. */
const STREAM_QUEUE_CAP = 256;

type Frame = { id: number; ok?: unknown; err?: string; item?: unknown; done?: boolean };

function entry(name: OperationName): OperationEntry {
  const found = MANIFEST.operations.find((operation) => operation.name === name);
  if (found === undefined) throw new LunaSdkError('unsupported', `unknown operation ${name}`, { operation: name });
  return found;
}

function check(schema: JsonSchema, value: unknown): string | null {
  return validate(schema, value, MANIFEST.$defs);
}

function wireError(operation: string, text: string): LunaSdkError {
  if (text.startsWith('unknown method: ')) return new LunaSdkError('unsupported', text, { operation });
  if (text.startsWith('bad params: ')) return new LunaSdkError('invalidRequest', text, { operation });
  if (text.startsWith('transport: ') || text === 'connection closed') {
    return new LunaSdkError('offline', text, { operation });
  }
  return new LunaSdkError('engine', text, { operation });
}

function invalid(operation: string, detail: string): LunaSdkError {
  return new LunaSdkError('invalidResponse', `${operation} returned an invalid response: ${detail}`, { operation });
}

function asSdkError(error: unknown, operation: string): LunaSdkError {
  if (error instanceof LunaSdkError) return error;
  return new LunaSdkError('offline', String(error), { operation });
}

/**
 * One server stream, bounded and cancellable. As in the Rust client, an
 * error before the stream is acknowledged fails the subscribe; afterwards
 * the stream simply ends and the caller resubscribes from its cursor.
 */
class Subscription {
  readonly id: number;
  #connection: Connection;
  #items: unknown[] = [];
  #waiter: (() => void) | null = null;
  #ended = false;
  #ready: { resolve: () => void; reject: (error: LunaSdkError) => void } | null;

  constructor(id: number, connection: Connection, ready: { resolve: () => void; reject: (error: LunaSdkError) => void }) {
    this.id = id;
    this.#connection = connection;
    this.#ready = ready;
  }

  markReady(): void {
    this.#ready?.resolve();
    this.#ready = null;
  }

  push(item: unknown): void {
    this.markReady();
    if (this.#ended) return;
    if (this.#items.length >= STREAM_QUEUE_CAP) {
      // A stalled consumer loses the stream instead of growing memory.
      this.#cancel();
      return;
    }
    this.#items.push(item);
    this.#wake();
  }

  end(error: LunaSdkError | null): void {
    if (this.#ready !== null) {
      if (error === null) this.#ready.resolve();
      else this.#ready.reject(error);
      this.#ready = null;
    }
    this.#ended = true;
    this.#wake();
  }

  /** The next item, or `null` once the stream has ended. */
  async next(): Promise<unknown> {
    for (;;) {
      if (this.#items.length > 0) return this.#items.shift();
      if (this.#ended) return null;
      await new Promise<void>((resolve) => {
        this.#waiter = resolve;
      });
    }
  }

  /** Cancel the server stream now and drop anything unread. Idempotent. */
  dispose(): void {
    this.#items = [];
    this.#cancel();
  }

  #cancel(): void {
    if (this.#ended) return;
    this.#connection.cancel(this.id);
    this.end(null);
  }

  #wake(): void {
    const waiter = this.#waiter;
    this.#waiter = null;
    waiter?.();
  }
}

type PendingCall = {
  resolve: (value: unknown) => void;
  reject: (error: LunaSdkError) => void;
  operation: string;
};

/** Request/stream multiplexing over one WebSocket, as the Rust `RpcClient`. */
class Connection {
  #socket: WebSocket;
  #nextId = 1;
  #pending = new Map<number, PendingCall | Subscription>();
  #closed = false;

  static open(url: string, timeoutMs: number): Promise<Connection> {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      const timer = setTimeout(() => {
        socket.close();
        reject(new LunaSdkError('offline', `transport: timed out dialing ${url}`));
      }, timeoutMs);
      socket.addEventListener('open', () => {
        clearTimeout(timer);
        resolve(new Connection(socket));
      }, { once: true });
      socket.addEventListener('error', () => {
        clearTimeout(timer);
        reject(new LunaSdkError('offline', `transport: cannot reach ${url}`));
      }, { once: true });
    });
  }

  constructor(socket: WebSocket) {
    this.#socket = socket;
    socket.addEventListener('message', (event) => this.#receive(String(event.data)));
    socket.addEventListener('close', () => this.#close());
    socket.addEventListener('error', () => this.#close());
  }

  call(operation: string, method: string, params: unknown, timeoutMs: number | undefined): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (this.#closed) {
        reject(new LunaSdkError('offline', 'connection closed', { operation }));
        return;
      }
      const id = this.#nextId++;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const settle = () => {
        if (timer !== undefined) clearTimeout(timer);
        this.#pending.delete(id);
      };
      this.#pending.set(id, {
        operation,
        resolve: (value) => {
          settle();
          resolve(value);
        },
        reject: (error) => {
          settle();
          reject(error);
        },
      });
      if (timeoutMs !== undefined) {
        // Like the Rust client, a local timeout forgets the reply slot; it
        // never cancels a mutation that may already be durable.
        timer = setTimeout(() => {
          this.#pending.delete(id);
          reject(new LunaSdkError('timeout', `${operation} timed out after ${timeoutMs}ms`, { operation }));
        }, timeoutMs);
      }
      this.#send({ id, method, params });
    });
  }

  /** Resolves once the server acknowledges the stream (first item, readiness or end). */
  subscribe(operation: string, method: string, params: unknown): Promise<Subscription> {
    return new Promise((resolve, reject) => {
      if (this.#closed) {
        reject(new LunaSdkError('offline', 'connection closed', { operation }));
        return;
      }
      const id = this.#nextId++;
      const subscription: Subscription = new Subscription(id, this, {
        resolve: () => resolve(subscription),
        reject: (error) => reject(error),
      });
      this.#pending.set(id, subscription);
      this.#send({ id, method, params });
    });
  }

  cancel(id: number): void {
    if (this.#pending.delete(id) && !this.#closed) this.#send({ id, cancel: true });
  }

  close(): void {
    this.#socket.close();
    this.#close();
  }

  #send(frame: unknown): void {
    this.#socket.send(JSON.stringify(frame));
  }

  #receive(text: string): void {
    for (const line of text.split('\n')) {
      if (line.trim() === '') continue;
      let frame: Frame;
      try {
        frame = JSON.parse(line) as Frame;
      } catch {
        continue;
      }
      // A frame that breaks the published frame contract is dropped, as the Rust client does.
      if (check(MANIFEST.frames.server, frame) !== null) continue;
      this.#route(frame);
    }
  }

  #route(frame: Frame): void {
    const pending = this.#pending.get(frame.id);
    if (pending === undefined) return;
    if (pending instanceof Subscription) {
      if (frame.err !== undefined) {
        this.#pending.delete(frame.id);
        pending.end(wireError('stream', frame.err));
      } else if (Object.hasOwn(frame, 'item')) {
        pending.push(frame.item);
      } else if (frame.done === true) {
        this.#pending.delete(frame.id);
        pending.end(null);
      } else {
        pending.markReady();
      }
      return;
    }
    if (frame.err !== undefined) pending.reject(wireError(pending.operation, frame.err));
    else if (Object.hasOwn(frame, 'ok')) pending.resolve(frame.ok);
  }

  #close(): void {
    if (this.#closed) return;
    this.#closed = true;
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const waiting of pending) {
      if (waiting instanceof Subscription) waiting.end(new LunaSdkError('offline', 'connection closed'));
      else waiting.reject(new LunaSdkError('offline', 'connection closed', { operation: waiting.operation }));
    }
  }
}

export class LunaClient {
  #connection: Connection;
  #timeoutMs: number | undefined;
  /** The negotiated engine API; `null` for an engine older than `DescribeApi`. */
  api: ApiDescription | null = null;

  private constructor(connection: Connection, timeoutMs: number | undefined) {
    this.#connection = connection;
    this.#timeoutMs = timeoutMs;
  }

  /** Dial a local engine, require a LUNA owner engine and negotiate its API. */
  static async connect(options: ConnectOptions): Promise<{ client: LunaClient; engine: EngineInfo }> {
    const url = options.url ?? `ws://127.0.0.1:${options.port ?? 59778}`;
    const client = new LunaClient(await Connection.open(url, options.timeoutMs ?? 5000), options.timeoutMs);
    const engine = await client.engineInfo();
    if (!(engine.capabilities ?? []).includes(OWNER_IPC)) {
      client.close();
      throw new LunaSdkError('incompatible', 'local listener is not a compatible LUNA engine');
    }
    await client.negotiate();
    return { client, engine };
  }

  /** Read the engine's API and refuse another contract version. */
  async negotiate(): Promise<ApiDescription | null> {
    let api: ApiDescription;
    try {
      api = await this.describeApi();
    } catch (error) {
      if (error instanceof LunaSdkError && error.kind === 'unsupported') return null;
      throw error;
    }
    if (api.apiVersion !== MANIFEST.apiVersion) {
      throw new LunaSdkError(
        'incompatible',
        `engine ${api.engineVersion} speaks API ${api.apiVersion}; this SDK (${MANIFEST.sdkVersion}) speaks API ${MANIFEST.apiVersion}`,
      );
    }
    this.api = api;
    return api;
  }

  #require(operation: OperationEntry): void {
    if (this.api === null) return;
    const served = this.api.operations.find((candidate) => candidate.id === operation.id);
    if (served === undefined) {
      throw new LunaSdkError('unsupported', `unknown method: ${operation.method}`, { operation: operation.name });
    }
    if (served.version !== operation.version) {
      throw new LunaSdkError(
        'incompatible',
        `engine serves ${operation.id}@${served.version}; this SDK calls @${operation.version}`,
      );
    }
  }

  /** Call a unary operation; its params and reply are validated against their schemas. */
  async call<N extends OperationName>(name: N, params: Operations[N]['params']): Promise<Operations[N]['output']> {
    const operation = entry(name);
    this.#require(operation);
    const problem = check(operation.params, params);
    if (problem !== null) {
      throw new LunaSdkError('invalidRequest', `bad params: ${problem}`, { operation: name });
    }
    const value = await this.#connection.call(name, operation.method, params, this.#timeoutMs);
    const invalidReply = check(operation.output, value);
    if (invalidReply !== null) throw invalid(name, invalidReply);
    return value as Operations[N]['output'];
  }

  describeApi(): Promise<ApiDescription> {
    return this.call('describeApi', {});
  }

  engineInfo(): Promise<EngineInfo> {
    return this.call('engineInfo', {});
  }

  /** The task's snapshot, or with `after` a bounded page of its durable events. */
  async getTaskCommandSnapshot(taskId: string, after?: number, targetDeviceId?: string): Promise<TaskCommandRead> {
    const read = await this.call('getTaskCommandSnapshot', {
      taskId,
      ...(after === undefined ? {} : { afterTaskSequence: after, limit: 100 }),
      ...(targetDeviceId === undefined ? {} : { targetDeviceId }),
    });
    validateRead('getTaskCommandSnapshot', taskId, after, read);
    return read;
  }

  /** A command's durable receipt; `null` means the engine never admitted it. */
  async getTaskCommandReceipt(commandId: string, targetDeviceId?: string): Promise<CommandReceipt | null> {
    const receipt = await this.call('getTaskCommandReceipt', {
      commandId,
      ...(targetDeviceId === undefined ? {} : { targetDeviceId }),
    });
    if (receipt !== null && receipt.commandId !== commandId) {
      throw invalid('getTaskCommandReceipt', `asked for command ${commandId}, got ${receipt.commandId}`);
    }
    return receipt;
  }

  /**
   * Admit one command and return its durable receipt. A refusal is a receipt,
   * and `pending` is not success. A lost reply is read back by the same id;
   * only when no receipt exists is the identical envelope sent once more.
   * If the receipt cannot be read either, this throws `unknownOutcome`.
   */
  async executeTaskCommand(input: TaskCommandInput): Promise<CommandReceipt> {
    const operation = entry('executeTaskCommand');
    this.#require(operation);
    const commandId = input.commandId ?? crypto.randomUUID();
    const fences = MANIFEST.intents.find((intent) => intent.kind === input.intent.kind)?.fences;
    if (fences === undefined) {
      throw new LunaSdkError('invalidRequest', `bad params: unpublished intent ${input.intent.kind}`, {
        operation: operation.name,
      });
    }
    const snapshot =
      input.reviewed ?? (await this.getTaskCommandSnapshot(input.taskId, undefined, input.targetDeviceId)).snapshot;
    const params: ExecuteTaskCommandParams = {
      commandId,
      actorId: input.actor.actorId,
      clientId: input.actor.clientId,
      clientSessionId: input.actor.clientSessionId,
      taskId: input.taskId,
      intent: input.intent,
    };
    if (fences.taskRevision) params.expectedTaskRevision = snapshot.taskRevision;
    if (fences.bindingGeneration && snapshot.bindingGeneration != null) {
      params.expectedBindingGeneration = snapshot.bindingGeneration;
    }
    if (fences.turn && snapshot.activeTurnId != null) params.expectedTurnId = snapshot.activeTurnId;
    if (input.issuedAtMs !== undefined) params.issuedAtMs = input.issuedAtMs;
    if (input.targetDeviceId !== undefined) params.targetDeviceId = input.targetDeviceId;
    const problem = check(operation.params, params);
    if (problem !== null) {
      throw new LunaSdkError('invalidRequest', `bad params: ${problem}`, { operation: operation.name });
    }
    const attempt = await this.#submit(operation, params).then(
      (receipt) => ({ receipt }),
      (error: unknown) => ({ error: asSdkError(error, operation.name) }),
    );
    if ('receipt' in attempt) return attempt.receipt;
    const first = attempt.error;
    const recovered = await this.#readBack(params, input.targetDeviceId);
    if (recovered !== null) return recovered;
    try {
      return await this.#submit(operation, params);
    } catch {
      // No receipt after a second failure means it was not admitted.
      const last = await this.#readBack(params, input.targetDeviceId);
      if (last !== null) return last;
      throw first;
    }
  }

  async #submit(operation: OperationEntry, params: ExecuteTaskCommandParams): Promise<CommandReceipt> {
    const value = await this.#connection.call(operation.name, operation.method, params, this.#timeoutMs);
    const problem = check(operation.output, value);
    if (problem !== null) throw invalid(operation.name, problem);
    const receipt = value as CommandReceipt;
    checkReceipt(operation.name, params, receipt);
    return receipt;
  }

  async #readBack(params: ExecuteTaskCommandParams, targetDeviceId: string | undefined): Promise<CommandReceipt | null> {
    try {
      const receipt = await this.getTaskCommandReceipt(params.commandId, targetDeviceId);
      if (receipt !== null) checkReceipt('getTaskCommandReceipt', params, receipt);
      return receipt;
    } catch (error) {
      const source = asSdkError(error, 'getTaskCommandReceipt');
      throw new LunaSdkError('unknownOutcome', `outcome of command ${params.commandId} is unknown: ${source.message}`, {
        operation: 'executeTaskCommand',
        commandId: params.commandId,
        cause: source,
      });
    }
  }

  /** Rename one task through the published intent, fenced from its current snapshot. */
  async renameTask(input: {
    actor: Actor;
    taskId: string;
    title: string;
    commandId?: string;
    /** Fence on this revision instead of the current one. */
    expectedTaskRevision?: number;
  }): Promise<CommandReceipt> {
    let reviewed: TaskCommandSnapshot | undefined;
    if (input.expectedTaskRevision !== undefined) {
      reviewed = {
        ...(await this.getTaskCommandSnapshot(input.taskId)).snapshot,
        taskRevision: input.expectedTaskRevision,
      };
    }
    return this.executeTaskCommand({
      actor: input.actor,
      taskId: input.taskId,
      intent: { kind: 'rename', title: input.title },
      ...(input.commandId === undefined ? {} : { commandId: input.commandId }),
      ...(reviewed === undefined ? {} : { reviewed }),
    });
  }

  /** Poll a receipt until it is terminal or `timeoutMs` passes; still `pending` on timeout. */
  async waitForTerminal(receipt: CommandReceipt, timeoutMs: number, targetDeviceId?: string): Promise<CommandReceipt> {
    const deadline = Date.now() + timeoutMs;
    let current = receipt;
    while (current.status === 'pending' && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 250));
      current = (await this.getTaskCommandReceipt(current.commandId, targetDeviceId)) ?? current;
    }
    return current;
  }

  /** Watch a task's commands from `after` (or its current snapshot). `dispose()` cancels it. */
  async watchTaskCommands(taskId: string, options: { after?: number; targetDeviceId?: string } = {}): Promise<TaskCommandWatch> {
    const operation = entry('watchTaskCommands');
    this.#require(operation);
    const params = {
      taskId,
      ...(options.after === undefined ? {} : { afterTaskSequence: options.after, limit: 100 }),
      ...(options.targetDeviceId === undefined ? {} : { targetDeviceId: options.targetDeviceId }),
    };
    const problem = check(operation.params, params);
    if (problem !== null) {
      throw new LunaSdkError('invalidRequest', `bad params: ${problem}`, { operation: operation.name });
    }
    const subscription = await this.#connection.subscribe(operation.name, operation.method, params);
    return new TaskCommandWatch(this, subscription, operation, taskId, options.after, options.targetDeviceId);
  }

  close(): void {
    this.#connection.close();
  }
}

/**
 * A task-command watch that never delivers a gap: the same cursor rules as
 * the Rust `TaskCommandWatch`. Terminal receipts advance the cursor in task
 * sequence; a skipped sequence is filled from the durable page first.
 */
export class TaskCommandWatch implements AsyncIterable<TaskCommandUpdate> {
  #client: LunaClient;
  #subscription: Subscription;
  #operation: OperationEntry;
  #taskId: string;
  #requestedAfter: number | undefined;
  #targetDeviceId: string | undefined;
  #cursor: number | null = null;
  #ready: TaskCommandUpdate[] = [];

  constructor(
    client: LunaClient,
    subscription: Subscription,
    operation: OperationEntry,
    taskId: string,
    after: number | undefined,
    targetDeviceId: string | undefined,
  ) {
    this.#client = client;
    this.#subscription = subscription;
    this.#operation = operation;
    this.#taskId = taskId;
    this.#requestedAfter = after;
    this.#targetDeviceId = targetDeviceId;
  }

  /** The task sequence of the last event delivered (or of the snapshot). */
  get cursor(): number | null {
    return this.#cursor;
  }

  /** The next update, or `null` once the stream ends. */
  async next(): Promise<TaskCommandUpdate | null> {
    for (;;) {
      const ready = this.#ready.shift();
      if (ready !== undefined) return ready;
      const value = await this.#subscription.next();
      if (value === null) return null;
      const problem = check(this.#operation.output, value);
      if (problem !== null) throw invalid(this.#operation.name, problem);
      await this.#accept(value as TaskCommandWatchItem);
    }
  }

  /** Read durable events past the cursor now (after a reconnect or a long idle). */
  async resync(): Promise<void> {
    if (this.#cursor !== null) await this.#repair(this.#cursor, undefined);
  }

  /** Cancel the engine's stream now. */
  dispose(): void {
    this.#subscription.dispose();
  }

  async *[Symbol.asyncIterator](): AsyncIterator<TaskCommandUpdate> {
    try {
      for (;;) {
        const update = await this.next();
        if (update === null) return;
        yield update;
      }
    } finally {
      this.dispose();
    }
  }

  async #accept(item: TaskCommandWatchItem): Promise<void> {
    if (item.kind === 'snapshot') {
      if (this.#cursor !== null) throw invalid(this.#operation.name, 'a second snapshot arrived');
      const read = item.read;
      validateRead(this.#operation.name, this.#taskId, this.#requestedAfter, read);
      if (read.reloadRequired) {
        this.#cursor = read.snapshot.taskSequence;
        this.#ready.push({ kind: 'reload', snapshot: read.snapshot });
        return;
      }
      const events = [...(read.events ?? [])];
      let cursor = events.at(-1)?.taskSequence ?? this.#requestedAfter ?? read.snapshot.taskSequence;
      let snapshot = read.snapshot;
      let hasMore = read.hasMore ?? false;
      while (hasMore) {
        const page = await this.#client.getTaskCommandSnapshot(this.#taskId, cursor, this.#targetDeviceId);
        if (page.reloadRequired) {
          this.#cursor = page.snapshot.taskSequence;
          this.#ready.push({ kind: 'reload', snapshot: page.snapshot });
          return;
        }
        const pageEvents = page.events ?? [];
        cursor = pageEvents.at(-1)?.taskSequence ?? cursor;
        hasMore = (page.hasMore ?? false) && pageEvents.length > 0;
        events.push(...pageEvents);
        snapshot = page.snapshot;
      }
      const complete: TaskCommandRead = { ...read, snapshot, events, hasMore: false };
      delete complete.nextTaskSequence;
      if (events.length > 0) complete.nextTaskSequence = events[events.length - 1].taskSequence;
      this.#cursor = cursor;
      this.#ready.push({ kind: 'snapshot', read: complete });
      return;
    }
    const receipt = item.receipt;
    if (this.#cursor === null) throw invalid(this.#operation.name, 'a receipt arrived before the snapshot');
    if (receipt.taskId !== this.#taskId) {
      throw invalid(this.#operation.name, `receipt ${receipt.commandId} is for task ${receipt.taskId}`);
    }
    if (receipt.status === 'pending' || receipt.eventSequence == null) {
      this.#ready.push({ kind: 'progress', receipt });
      return;
    }
    const sequence = receipt.snapshot.taskSequence;
    if (sequence <= this.#cursor) return;
    if (sequence > this.#cursor + 1) await this.#repair(this.#cursor, sequence);
    if (this.#cursor !== null && sequence > this.#cursor) {
      this.#cursor = sequence;
      this.#ready.push({ kind: 'event', receipt });
    }
  }

  async #repair(from: number, until: number | undefined): Promise<void> {
    let cursor = from;
    for (;;) {
      const page = await this.#client.getTaskCommandSnapshot(this.#taskId, cursor, this.#targetDeviceId);
      if (page.reloadRequired) {
        this.#cursor = page.snapshot.taskSequence;
        this.#ready.push({ kind: 'reload', snapshot: page.snapshot });
        return;
      }
      const events = page.events ?? [];
      const hasMore = (page.hasMore ?? false) && events.length > 0;
      for (const event of events) {
        if (until !== undefined && event.taskSequence >= until) {
          // The receipt that revealed the gap is delivered by its caller.
          this.#cursor = cursor;
          return;
        }
        cursor = event.taskSequence;
        this.#ready.push({ kind: 'event', receipt: event.receipt });
      }
      this.#cursor = cursor;
      if (!hasMore || (until !== undefined && cursor + 1 >= until)) return;
    }
  }
}

function checkReceipt(operation: string, params: ExecuteTaskCommandParams, receipt: CommandReceipt): void {
  if (receipt.commandId !== params.commandId || receipt.taskId !== params.taskId) {
    throw invalid(
      operation,
      `receipt is for command ${receipt.commandId} on task ${receipt.taskId}, not ${params.commandId} on ${params.taskId}`,
    );
  }
}

/** A page is for the requested task, in increasing order, after its cursor. */
function validateRead(operation: string, taskId: string, after: number | undefined, read: TaskCommandRead): void {
  if (read.snapshot.taskId !== taskId) throw invalid(operation, `snapshot is for task ${read.snapshot.taskId}`);
  let previous = after ?? 0;
  for (const event of read.events ?? []) {
    if (event.receipt.taskId !== taskId || event.commandId !== event.receipt.commandId || event.taskSequence <= previous) {
      throw invalid(operation, `event ${event.commandId} (task sequence ${event.taskSequence}) is out of order or for another task`);
    }
    previous = event.taskSequence;
  }
}
