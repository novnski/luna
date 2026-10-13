// Gemini CLI provider for LUNA (CORE-08 example), written the way a third
// party would write it: Node.js built-ins plus the public @luna/sdk/provider
// module, which scripts/pack.mjs vendors into the package. Nothing here
// comes from LUNA's engine.
//
// - Discovery: the `gemini` executable LUNA's environment can reach (PATH,
//   Homebrew and npm locations, then the login shell), its version from its
//   own package.json, and its models from the installed CLI's own catalog
//   module (@google/gemini-cli-core/dist/src/config/models.js).
// - A run: `gemini --output-format stream-json --approval-mode yolo
//   [--model M] [--resume S]` in the task's checkout, with the prompt on
//   stdin (never in argv, so it does not show in process listings).
// - Stop: terminate the CLI's whole process tree, then report `interrupted`
//   once it has exited.
// - A run ends only after the CLI's `result` frame and its exit, so its
//   session file is written before the next turn resumes it.

import { type ChildProcess, execFile, spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { access, readFile, realpath } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { delimiter, dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';

import {
  type ProviderModel,
  ProviderRejection,
  type ProviderStatus,
  type RunContext,
  type RunStart,
  type ToolCallShape,
  serveProvider,
} from '#luna-sdk/provider';

/** Must equal the contribution id in luna-plugin.json and luna-provider.json. */
const PROVIDER_ID = 'novnski.gemini/gemini-cli';
const CLI_PACKAGE = '@google/gemini-cli';
const CORE_PACKAGE = '@google/gemini-cli-core';
const LOOKUP_TIMEOUT_MS = 3000;
const STOP_GRACE_MS = 1500;
const STDERR_TAIL_CHARS = 1200;

interface Cli {
  /** The path found on the search path. */
  path: string;
  realPath: string;
  /** The directory of the CLI's own package.json. */
  root: string;
  version: string;
}

interface ModelCatalog {
  VALID_GEMINI_MODELS?: Iterable<unknown>;
  PREVIEW_GEMINI_MODEL_AUTO?: unknown;
  DEFAULT_GEMINI_MODEL_AUTO?: unknown;
  getDisplayString?: (model: string) => string;
  isActiveModel?: (model: string) => boolean;
  isPreviewModel?: (model: string) => boolean;
}

const home = process.env.HOME ?? '';
let located: Promise<Cli | string> | undefined;

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function str(value: unknown): string | undefined {
  return typeof value === 'string' && value !== '' ? value : undefined;
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

async function executable(path: string): Promise<boolean> {
  try {
    await access(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function capture(program: string, args: string[], timeoutMs: number): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(program, args, { timeout: timeoutMs, encoding: 'utf8' }, (error, stdout) => {
      if (error) reject(error);
      else resolve(stdout);
    });
  });
}

/** Where `gemini` is, once per worker (one worker serves one run). */
function cli(): Promise<Cli | string> {
  located ??= locate();
  return located;
}

async function locate(): Promise<Cli | string> {
  const dirs = [
    ...(process.env.PATH ?? '').split(delimiter),
    '/opt/homebrew/bin',
    '/usr/local/bin',
    join(home, '.local', 'bin'),
    join(home, '.npm-global', 'bin'),
    join(home, '.volta', 'bin'),
  ].filter((dir) => dir !== '');
  for (const dir of dirs) {
    const candidate = join(dir, 'gemini');
    if (await executable(candidate)) return inspect(candidate);
  }
  const shell = process.env.SHELL;
  if (shell !== undefined && (await executable(shell))) {
    try {
      const found = (await capture(shell, ['-lc', 'command -v gemini'], LOOKUP_TIMEOUT_MS)).trim().split('\n').pop();
      if (found !== undefined && found.startsWith('/') && (await executable(found))) return inspect(found);
    } catch {
      // Not found by the login shell either.
    }
  }
  return 'Gemini CLI (gemini) was not found on PATH, in /opt/homebrew/bin or /usr/local/bin, or by your login shell. Install it with `brew install gemini-cli` or `npm install -g @google/gemini-cli`.';
}

/** Confirm that `path` is an installed Gemini CLI package and read its version. */
async function inspect(path: string): Promise<Cli | string> {
  const real = await realpath(path);
  let dir = dirname(real);
  for (let depth = 0; depth < 8; depth += 1) {
    try {
      const pkg = JSON.parse(await readFile(join(dir, 'package.json'), 'utf8')) as { name?: unknown; version?: unknown };
      if (pkg.name === CLI_PACKAGE && typeof pkg.version === 'string') {
        return { path, realPath: real, root: dir, version: pkg.version };
      }
    } catch {
      // Keep looking upwards.
    }
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return `${path} is not an installed ${CLI_PACKAGE} package, so its models and protocol cannot be confirmed`;
}

async function status(): Promise<ProviderStatus> {
  const found = await cli();
  if (typeof found === 'string') return { available: false, unavailableReason: found };
  return { available: true, executable: { path: found.realPath, version: found.version } };
}

/** The installed CLI's own model catalog: its auto aliases (its default first), then its active models. */
async function models(): Promise<ProviderModel[]> {
  const found = await cli();
  if (typeof found === 'string') throw new Error(found);
  let catalog: ModelCatalog;
  try {
    const resolver = createRequire(join(found.root, 'package.json'));
    const core = dirname(resolver.resolve(`${CORE_PACKAGE}/package.json`));
    catalog = (await import(pathToFileURL(join(core, 'dist', 'src', 'config', 'models.js')).href)) as ModelCatalog;
  } catch (error) {
    throw new Error(`could not read the model catalog of Gemini CLI ${found.version}: ${message(error)}`);
  }
  const ids: string[] = [];
  for (const alias of [catalog.PREVIEW_GEMINI_MODEL_AUTO, catalog.DEFAULT_GEMINI_MODEL_AUTO]) {
    if (typeof alias === 'string' && !ids.includes(alias)) ids.push(alias);
  }
  for (const model of catalog.VALID_GEMINI_MODELS ?? []) {
    if (typeof model !== 'string' || ids.includes(model)) continue;
    if (catalog.isActiveModel !== undefined && !catalog.isActiveModel(model)) continue;
    ids.push(model);
  }
  if (ids.length === 0) throw new Error(`Gemini CLI ${found.version} lists no models`);
  return ids.map((id, index): ProviderModel => {
    const row: ProviderModel = { id, label: catalog.getDisplayString?.(id) ?? id };
    if (index === 0) row.description = `Gemini CLI ${found.version} default`;
    else if (catalog.isPreviewModel?.(id) === true) row.description = 'Preview';
    return row;
  });
}

/** Every descendant of `root`, deepest first. */
async function descendants(root: number): Promise<number[]> {
  let listing: string;
  try {
    listing = await capture('/bin/ps', ['-axo', 'pid=,ppid='], LOOKUP_TIMEOUT_MS);
  } catch {
    return [];
  }
  const children = new Map<number, number[]>();
  for (const line of listing.split('\n')) {
    const [pid, ppid] = line.trim().split(/\s+/).map(Number);
    if (!Number.isInteger(pid) || !Number.isInteger(ppid)) continue;
    const list = children.get(ppid) ?? [];
    list.push(pid);
    children.set(ppid, list);
  }
  const found: number[] = [];
  const queue = [root];
  while (queue.length > 0) {
    const next = queue.shift()!;
    for (const child of children.get(next) ?? []) {
      if (!found.includes(child)) {
        found.push(child);
        queue.push(child);
      }
    }
  }
  return found.reverse();
}

function signalAll(pids: number[], signal: NodeJS.Signals): void {
  for (const pid of pids) {
    try {
      process.kill(pid, signal);
    } catch {
      // Already gone.
    }
  }
}

/** How LUNA should show one of Gemini CLI's built-in tools. */
function shape(name: string, parameters: unknown): ToolCallShape | undefined {
  const input = record(parameters) ?? {};
  const text = (key: string): string | undefined => str(input[key]);
  switch (name) {
    case 'run_shell_command': {
      const command = text('command');
      return command === undefined ? undefined : { kind: 'exec', command };
    }
    case 'read_file': {
      const path = text('file_path') ?? text('absolute_path');
      return path === undefined ? undefined : { kind: 'read', path };
    }
    case 'write_file': {
      const path = text('file_path');
      return path === undefined ? undefined : { kind: 'write', path };
    }
    case 'replace': {
      const path = text('file_path');
      return path === undefined ? undefined : { kind: 'edit', path };
    }
    case 'grep_search':
    case 'search_file_content': {
      const pattern = text('pattern');
      return pattern === undefined ? undefined : { kind: 'search', pattern, path: text('dir_path') };
    }
    case 'glob': {
      const pattern = text('pattern');
      return pattern === undefined ? undefined : { kind: 'glob', pattern };
    }
    case 'google_web_search': {
      const query = text('query');
      return query === undefined ? undefined : { kind: 'webSearch', query };
    }
    default:
      return undefined;
  }
}

/** One `gemini` process for one LUNA run. */
class GeminiRun {
  readonly #child: ChildProcess;
  readonly #prompt: string;
  readonly #run: RunContext;
  #result: { ok: true } | { ok: false; error: string } | undefined;
  #stderr = '';
  #stopping = false;

  constructor(child: ChildProcess, prompt: string, run: RunContext) {
    this.#child = child;
    this.#prompt = prompt;
    this.#run = run;
  }

  follow(): void {
    const child = this.#child;
    // The CLI may exit before reading all of its input.
    child.stdin?.on('error', () => undefined);
    child.stdin?.end(this.#prompt);
    child.stderr?.setEncoding('utf8');
    child.stderr?.on('data', (chunk: string) => {
      this.#stderr = (this.#stderr + chunk).slice(-STDERR_TAIL_CHARS);
    });
    if (child.stdout !== null) {
      const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
      lines.on('line', (line) => this.#onLine(line));
    }
    child.once('error', (error) => {
      this.#result ??= { ok: false, error: `Gemini CLI failed: ${message(error)}` };
    });
    child.once('close', (code, signal) => this.#closed(code, signal));
    if (this.#run.signal.aborted) void this.#stop();
    else this.#run.signal.addEventListener('abort', () => void this.#stop(), { once: true });
  }

  #onLine(line: string): void {
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      return;
    }
    const event = record(parsed);
    if (event === undefined) return;
    const run = this.#run;
    switch (event.type) {
      case 'init': {
        const session = str(event.session_id);
        if (session !== undefined) run.session(session, str(event.model));
        break;
      }
      case 'message':
        if (event.role === 'assistant' && typeof event.content === 'string') run.text(event.content);
        break;
      case 'tool_use': {
        const id = str(event.tool_id);
        const name = str(event.tool_name);
        if (id !== undefined && name !== undefined) run.toolCall(id, name, shape(name, event.parameters), event.parameters);
        break;
      }
      case 'tool_result': {
        const id = str(event.tool_id);
        if (id !== undefined) {
          const failure = record(event.error);
          run.toolResult(id, event.status === 'error', str(event.output) ?? str(failure?.message));
        }
        break;
      }
      case 'error': {
        const text = str(event.message);
        if (text !== undefined) {
          if (event.severity === 'warning') run.notice(text);
          else run.error(text);
        }
        break;
      }
      case 'result': {
        const stats = record(event.stats);
        if (stats !== undefined && typeof stats.input_tokens === 'number' && typeof stats.output_tokens === 'number') {
          run.usage(stats.input_tokens, stats.output_tokens);
        }
        if (event.status === 'success') {
          this.#result = { ok: true };
        } else {
          const failure = record(event.error);
          this.#result = { ok: false, error: str(failure?.message) ?? 'Gemini CLI reported an error' };
        }
        break;
      }
      default:
        // `message` from the user (an echo) and anything newer.
        break;
    }
  }

  #closed(code: number | null, signal: NodeJS.Signals | null): void {
    if (this.#stopping) {
      this.#run.finish({ status: 'interrupted' });
      return;
    }
    if (this.#result?.ok === true && code === 0) {
      this.#run.finish({ status: 'completed' });
      return;
    }
    const exit = signal !== null ? `signal ${signal}` : `exit code ${String(code)}`;
    let reason: string;
    if (this.#result === undefined) reason = `Gemini CLI exited (${exit}) without a result`;
    else if (this.#result.ok) reason = `Gemini CLI reported success but exited with ${exit}`;
    else reason = this.#result.error;
    const tail = this.#stderr.trim();
    this.#run.finish({ status: 'failed', error: tail === '' ? reason : `${reason}\n${tail.slice(-600)}` });
  }

  async #stop(): Promise<void> {
    if (this.#stopping) return;
    this.#stopping = true;
    const pid = this.#child.pid;
    if (pid === undefined || this.#child.exitCode !== null || this.#child.signalCode !== null) return;
    // Snapshot the tree now: once the CLI exits, its children are reparented.
    const tree = [...(await descendants(pid)), pid];
    signalAll(tree, 'SIGTERM');
    setTimeout(() => signalAll(tree, 'SIGKILL'), STOP_GRACE_MS).unref();
  }
}

async function startRun(start: RunStart, run: RunContext): Promise<void> {
  const found = await cli();
  if (typeof found === 'string') throw new ProviderRejection(found);
  if (start.permissionMode !== 'full') {
    throw new ProviderRejection(
      'Gemini CLI runs here only with Full access (--approval-mode yolo); it has no automatic reviewer for Approve for me.',
    );
  }
  const args = ['--output-format', 'stream-json', '--approval-mode', 'yolo'];
  if (start.model !== undefined && start.model !== '') args.push('--model', start.model);
  if (start.resume !== undefined) args.push('--resume', start.resume);
  // npm-installed CLIs are `#!/usr/bin/env node` scripts: put a Node.js and
  // the CLI's own directory ahead of the minimal GUI PATH.
  const path = [dirname(found.path), dirname(process.execPath), process.env.PATH ?? '']
    .filter((dir) => dir !== '')
    .join(delimiter);
  const child = spawn(found.path, args, {
    cwd: start.cwd,
    env: { ...process.env, PATH: path, NO_COLOR: '1' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  try {
    await new Promise<void>((resolve, reject) => {
      child.once('spawn', () => resolve());
      child.once('error', reject);
    });
  } catch (error) {
    throw new ProviderRejection(`Gemini CLI could not be started in ${start.cwd}: ${message(error)}`);
  }
  new GeminiRun(child, start.prompt, run).follow();
}

serveProvider({
  providerId: PROVIDER_ID,
  optional: ['resume'],
  status,
  models,
  startRun,
});
