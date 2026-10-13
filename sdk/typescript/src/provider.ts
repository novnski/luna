// @luna/sdk/provider: write a LUNA provider extension worker (CORE-08).
//
// A provider package is a native extension (CORE-06 manifest) whose worker
// speaks worker protocol 1.0 (CORE-07) over its stdio and serves provider
// profile 1.0 on it. This module is the whole worker side: the hello, call
// dispatch with exactly one answer per call, cancels answered within the
// host's grace period, deadlines, bounded frames and event pages, and the
// run event queue LUNA long-polls. Call `serveProvider()` once.
//
// It is self-contained (Node.js built-ins only) so a package can vendor this
// one file, and it imports nothing from LUNA's engine. The static facts of a
// provider (its id, optional profiles and permission modes) belong in the
// package's `luna-provider.json`; this worker reports only what it finds at
// runtime (availability, its executable, its models, its runs).
//
// A native worker is not sandboxed: it runs with the user's account access.

import { randomUUID } from 'node:crypto';
import { createInterface } from 'node:readline';

export const WORKER_PROTOCOL = { major: 1, minor: 0 } as const;
export const PROVIDER_PROFILE = { major: 1, minor: 0 } as const;

/** Every provider profile 1.0 worker serves these. */
export const MANDATORY_OPERATIONS = [
  'provider.describe',
  'provider.models',
  'provider.run.start',
  'provider.run.events',
  'provider.run.stop',
] as const;
/** Served when the `questions` or `approvals` profile is declared. */
export const ANSWER_OPERATION = 'provider.run.answer';

const FRAME_LIMIT = 64 * 1024;
/** Budget for one events page, below the frame limit. */
const PAGE_BUDGET = 48 * 1024;
/** Text deltas are split into pieces of at most this many characters. */
const TEXT_CHUNK_CHARS = 4096;
const OUTPUT_LIMIT_CHARS = 16 * 1024;
const INPUT_LIMIT_BYTES = 8 * 1024;
const MESSAGE_LIMIT_CHARS = 2000;
/** Events LUNA has not read yet; past this the run fails instead of growing. */
const MAX_QUEUED_EVENTS = 4096;
const MAX_QUEUED_BYTES = 8 * 1024 * 1024;
/** Answer this long before the host's deadline so it never has to cancel. */
const DEADLINE_MARGIN_MS = 500;
const DEFAULT_DEADLINE_MS = 30_000;
const SHUTDOWN_GRACE_MS = 300;

export type PermissionMode = 'auto' | 'full';
export type OptionalProfile = 'resume' | 'questions' | 'approvals' | 'attachments';
export type ReasoningLevel =
  | 'minimal'
  | 'low'
  | 'medium'
  | 'high'
  | 'xhigh'
  | 'max'
  | 'ultra'
  | 'ultracode'
  | 'ultrathink';

export interface ProviderModel {
  id: string;
  label: string;
  description?: string;
  reasoningLevels?: ReasoningLevel[];
}

export interface ProviderStatus {
  available: boolean;
  /** Shown to the user when `available` is false. */
  unavailableReason?: string;
  /** The program this provider runs. */
  executable?: { path: string; version?: string };
}

/** `provider.run.start`, as LUNA sends it. */
export interface RunStart {
  prompt: string;
  cwd: string;
  model?: string;
  reasoning?: ReasoningLevel;
  modelOptions: Record<string, unknown>;
  permissionMode: PermissionMode;
  /** The provider session to continue (the `resume` profile only). */
  resume?: string;
  /** Staged image paths (the `attachments` profile only). */
  attachments: string[];
}

/** How LUNA should present a tool call; anything else is shown by name. */
export type ToolCallShape =
  | { kind: 'exec'; command: string }
  | { kind: 'read'; path: string }
  | { kind: 'write'; path: string }
  | { kind: 'edit'; path: string }
  | { kind: 'search'; pattern: string; path?: string }
  | { kind: 'glob'; pattern: string }
  | { kind: 'webFetch'; url: string }
  | { kind: 'webSearch'; query: string };

export interface Question {
  id: string;
  header?: string;
  question: string;
  options?: string[];
  multiSelect?: boolean;
}

export interface Answer {
  questionId: string;
  labels: string[];
}

export interface ApprovalDecision {
  decision: 'allow' | 'deny';
  reason?: string;
}

/** How a run ended. Only `completed` completes a turn in LUNA. */
export type RunTerminal =
  | { status: 'completed'; result?: string }
  | { status: 'interrupted' }
  | { status: 'failed'; error: string };

/**
 * Throw from `startRun` when nothing was started (the prompt never reached
 * the CLI). LUNA reports the message; any other error means delivery is
 * unknown and LUNA never sends the prompt again.
 */
export class ProviderRejection extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ProviderRejection';
  }
}

/** What a run reports. Events after `finish` are ignored. */
export interface RunContext {
  readonly runId: string;
  /** Aborted when LUNA stops the run or the worker shuts down. */
  readonly signal: AbortSignal;
  /** The provider's own session id; required before output when resuming. */
  session(sessionId: string, model?: string): void;
  text(text: string): void;
  reasoning(text: string): void;
  toolCall(id: string, name: string, call?: ToolCallShape, input?: unknown): void;
  toolResult(id: string, isError: boolean, output?: string): void;
  usage(inputTokens: number, outputTokens: number): void;
  /** A transient warning (shown beside the working indicator). */
  notice(message: string): void;
  /** A visible, non-fatal error. */
  error(message: string): void;
  /** The `questions` profile: `null` when dismissed or stopped. */
  ask(questions: Question[]): Promise<Answer[] | null>;
  /** The `approvals` profile: LUNA settles it by the chat's mode, never by asking. */
  approve(kind: string, subject: string): Promise<ApprovalDecision>;
  /** Report the end once the CLI has exited. */
  finish(terminal: RunTerminal): void;
  log(message: string, level?: 'debug' | 'info' | 'warn' | 'error'): void;
}

export interface ProviderWorker {
  /** The provider contribution id, exactly as in `luna-provider.json`. */
  providerId: string;
  /** The optional profiles declared in `luna-provider.json`. */
  optional?: OptionalProfile[];
  status(): Promise<ProviderStatus>;
  models(): Promise<ProviderModel[]>;
  /**
   * Start the CLI for one run and resolve once it is running. Report output
   * through `run` and end with `run.finish`. Stop it when `run.signal`
   * aborts and finish with `interrupted` after it has exited.
   */
  startRun(start: RunStart, run: RunContext): Promise<void>;
  /** Called once before the worker exits. */
  shutdown?(): Promise<void> | void;
}

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type JsonObject = { [key: string]: Json };

interface QueuedEvent {
  seq: number;
  bytes: number;
  event: JsonObject;
}

interface RunState {
  id: string;
  seq: number;
  queue: QueuedEvent[];
  queuedBytes: number;
  finished: boolean;
  /** The terminal event's seq once finished. */
  terminalSeq: number;
  abort: AbortController;
  waiters: Set<() => void>;
  answers: Map<string, (value: JsonObject) => void>;
}

interface PendingCall {
  /** Runs when the host cancels the call. */
  onCancel?: () => void;
}

function clip(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, max)}…`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function asString(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

class ProviderHost {
  readonly #worker: ProviderWorker;
  readonly #optional: Set<OptionalProfile>;
  readonly #pending = new Map<string, PendingCall>();
  readonly #runs = new Map<string, RunState>();
  #helloDone = false;
  #closing = false;

  constructor(worker: ProviderWorker) {
    this.#worker = worker;
    this.#optional = new Set(worker.optional ?? []);
  }

  start(): void {
    const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
    lines.on('line', (line) => this.#onLine(line));
    lines.on('close', () => void this.#shutdown());
  }

  #write(frame: JsonObject): boolean {
    const line = JSON.stringify(frame);
    if (Buffer.byteLength(line) > FRAME_LIMIT) return false;
    process.stdout.write(`${line}\n`);
    return true;
  }

  /** Exactly one answer per call; a cancelled or answered call stays quiet. */
  #respond(id: string, outcome: { result: Json } | { error: { code: string; message: string } }): void {
    if (!this.#pending.delete(id)) return;
    const frame: JsonObject =
      'result' in outcome
        ? { type: 'response', id, ok: true, result: outcome.result }
        : { type: 'response', id, ok: false, error: outcome.error };
    if (!this.#write(frame)) {
      this.#write({
        type: 'response',
        id,
        ok: false,
        error: { code: 'tooLarge', message: 'the answer is larger than a protocol frame' },
      });
    }
  }

  #fail(id: string, code: string, message: string): void {
    this.#respond(id, { error: { code, message: clip(message, MESSAGE_LIMIT_CHARS) } });
  }

  #log(message: string, level: string): void {
    this.#write({ type: 'log', level, message: clip(message, MESSAGE_LIMIT_CHARS) });
  }

  #onLine(line: string): void {
    let frame: unknown;
    try {
      frame = JSON.parse(line);
    } catch {
      return;
    }
    if (!isRecord(frame)) return;
    if (!this.#helloDone) {
      this.#hello(frame);
      return;
    }
    switch (frame.type) {
      case 'call':
        this.#call(frame);
        break;
      case 'cancel': {
        const id = asString(frame.id);
        if (id === undefined) return;
        const pending = this.#pending.get(id);
        if (pending === undefined) return;
        pending.onCancel?.();
        this.#fail(id, 'cancelled', 'the call was cancelled');
        break;
      }
      case 'shutdown':
        void this.#shutdown();
        break;
      default:
        // `hostResult` answers host calls, which provider profile 1.0 does not make.
        break;
    }
  }

  #hello(frame: Record<string, unknown>): void {
    const protocol = frame.protocol;
    const instance = frame.instance;
    if (frame.type !== 'hello' || !isRecord(protocol) || !isRecord(instance)) {
      process.stderr.write('provider worker: the first frame was not a hello\n');
      process.exit(1);
    }
    if (protocol.major !== WORKER_PROTOCOL.major) {
      process.stderr.write(`provider worker: LUNA speaks worker protocol ${String(protocol.major)}; this worker speaks ${WORKER_PROTOCOL.major}\n`);
      process.exit(1);
    }
    const operations: string[] = [...MANDATORY_OPERATIONS];
    if (this.#optional.has('questions') || this.#optional.has('approvals')) {
      operations.push(ANSWER_OPERATION);
    }
    this.#helloDone = true;
    this.#write({
      type: 'hello',
      id: String(frame.id),
      protocol: { major: WORKER_PROTOCOL.major, minor: WORKER_PROTOCOL.minor },
      extensionId: String(instance.extensionId),
      packageHash: String(instance.packageHash),
      operations,
    });
  }

  #call(frame: Record<string, unknown>): void {
    const id = asString(frame.id);
    const method = asString(frame.method);
    if (id === undefined || method === undefined) return;
    const params: Record<string, unknown> = isRecord(frame.params) ? frame.params : {};
    const deadlineMs = typeof frame.deadlineMs === 'number' ? frame.deadlineMs : DEFAULT_DEADLINE_MS;
    const pending: PendingCall = {};
    this.#pending.set(id, pending);
    const handle = async (): Promise<void> => {
      switch (method) {
        case 'provider.describe':
          return this.#describe(id);
        case 'provider.models':
          return this.#models(id);
        case 'provider.run.start':
          return this.#startRun(id, params);
        case 'provider.run.events':
          return this.#events(id, params, deadlineMs, pending);
        case 'provider.run.stop':
          return this.#stopRun(id, params);
        case ANSWER_OPERATION:
          return this.#answer(id, params);
        default:
          this.#fail(id, 'unknownOperation', `${method} is not a provider profile 1.0 operation`);
      }
    };
    handle().catch((error: unknown) => this.#fail(id, 'failed', errorMessage(error)));
  }

  async #describe(id: string): Promise<void> {
    const status = await this.#worker.status();
    const result: JsonObject = {
      profile: { major: PROVIDER_PROFILE.major, minor: PROVIDER_PROFILE.minor },
      providerId: this.#worker.providerId,
      available: status.available,
    };
    if (status.unavailableReason !== undefined) {
      result.unavailableReason = clip(status.unavailableReason, MESSAGE_LIMIT_CHARS);
    }
    if (status.executable !== undefined) {
      const executable: JsonObject = { path: status.executable.path };
      if (status.executable.version !== undefined) executable.version = status.executable.version;
      result.executable = executable;
    }
    this.#respond(id, { result });
  }

  async #models(id: string): Promise<void> {
    const models = await this.#worker.models();
    const rows: Json[] = models.map((model) => {
      const row: JsonObject = { id: model.id, label: model.label };
      if (model.description !== undefined) row.description = model.description;
      if (model.reasoningLevels !== undefined) row.reasoningLevels = [...model.reasoningLevels];
      return row;
    });
    this.#respond(id, { result: { models: rows } });
  }

  async #startRun(id: string, params: Record<string, unknown>): Promise<void> {
    const prompt = asString(params.prompt);
    const cwd = asString(params.cwd);
    const permissionMode: PermissionMode | undefined =
      params.permissionMode === 'auto' || params.permissionMode === 'full' ? params.permissionMode : undefined;
    if (prompt === undefined || cwd === undefined || permissionMode === undefined) {
      this.#fail(id, 'rejected', 'run.start needs a prompt, a cwd and a permission mode');
      return;
    }
    const start: RunStart = {
      prompt,
      cwd,
      model: asString(params.model),
      reasoning: asString(params.reasoning) as ReasoningLevel | undefined,
      modelOptions: isRecord(params.modelOptions) ? params.modelOptions : {},
      permissionMode,
      resume: asString(params.resume),
      attachments: Array.isArray(params.attachments)
        ? params.attachments.filter((item): item is string => typeof item === 'string')
        : [],
    };
    if (start.resume !== undefined && !this.#optional.has('resume')) {
      this.#fail(id, 'rejected', 'this provider does not declare the resume profile');
      return;
    }
    if (start.attachments.length > 0 && !this.#optional.has('attachments')) {
      this.#fail(id, 'rejected', 'this provider does not declare the attachments profile');
      return;
    }
    const run: RunState = {
      id: `run-${randomUUID()}`,
      seq: 0,
      queue: [],
      queuedBytes: 0,
      finished: false,
      terminalSeq: 0,
      abort: new AbortController(),
      waiters: new Set(),
      answers: new Map(),
    };
    this.#runs.set(run.id, run);
    try {
      await this.#worker.startRun(start, this.#context(run));
    } catch (error) {
      if (error instanceof ProviderRejection) {
        this.#runs.delete(run.id);
        this.#fail(id, 'rejected', error.message);
      } else {
        // Something may have started: LUNA treats delivery as unknown.
        this.#finish(run, { status: 'failed', error: errorMessage(error) });
        this.#fail(id, 'failed', errorMessage(error));
      }
      return;
    }
    this.#respond(id, { result: { runId: run.id } });
  }

  #run(id: string, params: Record<string, unknown>): RunState | undefined {
    const run = this.#runs.get(asString(params.runId) ?? '');
    if (run === undefined) this.#fail(id, 'unknownRun', 'there is no such run in this worker');
    return run;
  }

  async #events(id: string, params: Record<string, unknown>, deadlineMs: number, pending: PendingCall): Promise<void> {
    const run = this.#run(id, params);
    if (run === undefined) return;
    const after = typeof params.after === 'number' ? params.after : 0;
    // `after` acknowledges everything at or below it: LUNA applied those and
    // keeps one cursor per run that never moves back, so they are freed. A
    // repeated `after` (a lost reply) gets the same events again.
    while (run.queue.length > 0 && run.queue[0]!.seq <= after) {
      run.queuedBytes -= run.queue.shift()!.bytes;
    }
    const requested = typeof params.waitMs === 'number' ? params.waitMs : 0;
    const wait = Math.max(0, Math.min(requested, deadlineMs - DEADLINE_MARGIN_MS));
    if (run.queue.length === 0 && wait > 0) {
      await new Promise<void>((resolve) => {
        const wake = (): void => {
          clearTimeout(timer);
          run.waiters.delete(wake);
          resolve();
        };
        const timer = setTimeout(wake, wait);
        run.waiters.add(wake);
        pending.onCancel = wake;
      });
    }
    const events: Json[] = [];
    let bytes = 0;
    let done = false;
    for (const queued of run.queue) {
      if (events.length > 0 && bytes + queued.bytes > PAGE_BUDGET) break;
      events.push(queued.event);
      bytes += queued.bytes;
      if (queued.seq === run.terminalSeq) done = true;
    }
    this.#respond(id, { result: { events, done } });
  }

  async #stopRun(id: string, params: Record<string, unknown>): Promise<void> {
    const run = this.#run(id, params);
    if (run === undefined) return;
    if (run.finished) {
      this.#respond(id, { result: { stopping: false, finished: true } });
      return;
    }
    run.abort.abort('stop');
    this.#respond(id, { result: { stopping: true } });
  }

  async #answer(id: string, params: Record<string, unknown>): Promise<void> {
    const run = this.#run(id, params);
    if (run === undefined) return;
    const requestId = asString(params.requestId);
    const settle = requestId === undefined ? undefined : run.answers.get(requestId);
    if (settle === undefined) {
      this.#fail(id, 'unknownRequest', 'there is no such pending request');
      return;
    }
    run.answers.delete(requestId!);
    settle(params as JsonObject);
    this.#respond(id, { result: {} });
  }

  #push(run: RunState, event: JsonObject): void {
    if (run.finished) return;
    let line = JSON.stringify(event);
    if (Buffer.byteLength(line) > PAGE_BUDGET) {
      event = { type: 'error', message: `a ${String(event.type)} event was too large to send and was dropped` };
      line = JSON.stringify(event);
    }
    if (run.queue.length >= MAX_QUEUED_EVENTS || run.queuedBytes + line.length > MAX_QUEUED_BYTES) {
      run.abort.abort('overflow');
      this.#finish(run, { status: 'failed', error: 'LUNA stopped reading this run’s events' });
      return;
    }
    run.seq += 1;
    const queued: JsonObject = { seq: run.seq, ...event };
    const bytes = Buffer.byteLength(JSON.stringify(queued));
    run.queue.push({ seq: run.seq, bytes, event: queued });
    run.queuedBytes += bytes;
    for (const wake of [...run.waiters]) wake();
  }

  #finish(run: RunState, terminal: RunTerminal): void {
    if (run.finished) return;
    const event: JsonObject = { type: 'done', status: terminal.status };
    if (terminal.status === 'completed' && terminal.result !== undefined) event.result = terminal.result;
    if (terminal.status === 'failed') event.error = clip(terminal.error, MESSAGE_LIMIT_CHARS);
    // Push first, then seal, so the terminal event is the last one queued.
    this.#push(run, event);
    run.finished = true;
    run.terminalSeq = run.seq;
    for (const settle of run.answers.values()) settle({ cancelled: true });
    run.answers.clear();
  }

  #context(run: RunState): RunContext {
    const push = (event: JsonObject): void => this.#push(run, event);
    const optional = this.#optional;
    const request = (event: JsonObject): Promise<JsonObject> =>
      new Promise((resolve) => {
        const requestId = `request-${randomUUID()}`;
        run.answers.set(requestId, resolve);
        run.abort.signal.addEventListener('abort', () => {
          if (run.answers.delete(requestId)) resolve({ cancelled: true });
        }, { once: true });
        push({ ...event, requestId });
      });
    return {
      runId: run.id,
      signal: run.abort.signal,
      session: (sessionId, model) => {
        const event: JsonObject = { type: 'session', sessionId };
        if (model !== undefined) event.model = model;
        push(event);
      },
      text: (text) => {
        for (let at = 0; at < text.length; at += TEXT_CHUNK_CHARS) {
          push({ type: 'text', text: text.slice(at, at + TEXT_CHUNK_CHARS) });
        }
      },
      reasoning: (text) => {
        for (let at = 0; at < text.length; at += TEXT_CHUNK_CHARS) {
          push({ type: 'reasoning', text: text.slice(at, at + TEXT_CHUNK_CHARS) });
        }
      },
      toolCall: (id, name, call, input) => {
        const event: JsonObject = { type: 'toolCall', id, name };
        if (call !== undefined) event.call = call as unknown as JsonObject;
        if (input !== undefined) {
          const encoded = JSON.stringify(input);
          if (encoded !== undefined && Buffer.byteLength(encoded) <= INPUT_LIMIT_BYTES) {
            event.input = JSON.parse(encoded) as Json;
          }
        }
        push(event);
      },
      toolResult: (id, isError, output) => {
        const event: JsonObject = { type: 'toolResult', id, isError };
        if (output !== undefined) event.output = clip(output, OUTPUT_LIMIT_CHARS);
        push(event);
      },
      usage: (inputTokens, outputTokens) => push({ type: 'usage', inputTokens, outputTokens }),
      notice: (message) => push({ type: 'notice', message: clip(message, MESSAGE_LIMIT_CHARS) }),
      error: (message) => push({ type: 'error', message: clip(message, MESSAGE_LIMIT_CHARS) }),
      ask: async (questions) => {
        if (!optional.has('questions')) throw new Error('the questions profile is not declared');
        const rows: Json[] = questions.map((question) => ({
          id: question.id,
          header: question.header ?? '',
          question: question.question,
          options: [...(question.options ?? [])],
          multiSelect: question.multiSelect ?? false,
        }));
        const answer = await request({ type: 'inputRequest', questions: rows });
        const answered = answer.answers;
        if (answer.cancelled === true || !Array.isArray(answered)) return null;
        const answers: Answer[] = [];
        for (const item of answered) {
          if (!isRecord(item)) continue;
          const labels = item.labels;
          answers.push({
            questionId: String(item.questionId),
            labels: Array.isArray(labels) ? labels.map(String) : [],
          });
        }
        return answers;
      },
      approve: async (kind, subject) => {
        if (!optional.has('approvals')) throw new Error('the approvals profile is not declared');
        const answer = await request({ type: 'approvalRequest', kind, subject });
        const reason = asString(answer.reason);
        if (answer.decision === 'allow') return { decision: 'allow' };
        return reason === undefined ? { decision: 'deny' } : { decision: 'deny', reason };
      },
      finish: (terminal) => this.#finish(run, terminal),
      log: (message, level = 'info') => this.#log(message, level),
    };
  }

  async #shutdown(): Promise<void> {
    if (this.#closing) return;
    this.#closing = true;
    for (const run of this.#runs.values()) {
      if (!run.finished) run.abort.abort('shutdown');
    }
    try {
      await Promise.race([
        Promise.resolve(this.#worker.shutdown?.()),
        new Promise((resolve) => setTimeout(resolve, SHUTDOWN_GRACE_MS)),
      ]);
    } finally {
      process.exit(0);
    }
  }
}

/** Serve `worker` on this process's stdio until LUNA ends the instance. */
export function serveProvider(worker: ProviderWorker): void {
  new ProviderHost(worker).start();
}
