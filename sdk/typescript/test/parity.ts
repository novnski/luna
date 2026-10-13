// CORE-05 E03/E07 driver for the independent TypeScript client. The Rust
// harness (apps/luna/tests/sdk_parity.rs) hosts a real engine, injects one
// lost reply, one corrupt reply and one hidden watch receipt, and compares
// everything this prints with the engine's durable records and the Rust
// clients' receipts. It never decides pass or fail itself.
//
//   node sdk/typescript/test/parity.ts '<input json>'
//
// Lines on stdout: {"event":"disposed",...}, then (after the harness writes
// one line to stdin) {"event":"result",...}; or {"event":"error",...}.

import { createInterface } from 'node:readline';

import { LunaClient, LunaSdkError, newActor } from '../src/index.ts';
import type { TaskCommandUpdate } from '../src/index.ts';

interface Input {
  port: number;
  taskId: string;
  title: string;
  staleTitle: string;
  lostCommandId: string;
  corruptCommandId: string;
  gapCommandId: string;
}

const input = JSON.parse(process.argv[2] ?? '{}') as Input;
const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const emit = (value: unknown) => process.stdout.write(`${JSON.stringify(value)}\n`);

try {
  const { client, engine } = await LunaClient.connect({ port: input.port, timeoutMs: 5000 });
  const actor = newActor(engine.deviceId, 'luna-sdk-ts');
  const { taskId } = input;
  const allowed = await client.renameTask({ actor, taskId, title: input.title, commandId: 'e07-ts-allowed' });
  const denied = await client.renameTask({
    actor,
    taskId,
    title: input.staleTitle,
    commandId: 'e07-ts-denied',
    expectedTaskRevision: 0,
  });
  // The harness executes this command and loses its reply.
  const recovered = await client.renameTask({ actor, taskId, title: input.title, commandId: input.lostCommandId });
  let invalid: { kind: string; message: string } | null = null;
  try {
    await client.getTaskCommandReceipt(input.corruptCommandId);
  } catch (error) {
    if (!(error instanceof LunaSdkError)) throw error;
    invalid = { kind: error.kind, message: error.message };
  }
  const before = (await client.getTaskCommandSnapshot(taskId)).snapshot.taskSequence;
  const watch = await client.watchTaskCommands(taskId, { after: before });
  const updates: Array<TaskCommandUpdate | null> = [await watch.next()];
  // The harness hides this receipt from every stream; the watch must repair it.
  await client.renameTask({ actor, taskId, title: `${input.title} gap`, commandId: input.gapCommandId });
  await client.renameTask({ actor, taskId, title: `${input.title} after gap`, commandId: 'e07-ts-after-gap' });
  // Accepted (pending) receipts arrive as progress; collect the two events.
  for (let events = 0; events < 2; ) {
    const update = await watch.next();
    if (update === null) break;
    updates.push(update);
    if (update.kind === 'event') events++;
  }
  const cursor = watch.cursor;
  watch.dispose();
  emit({ event: 'disposed', cursor });
  await lines.next();
  // The connection outlives the disposed watch.
  const afterDispose = (await client.engineInfo()).deviceId;
  client.close();
  emit({
    event: 'result',
    api: client.api,
    allowed,
    denied,
    recovered,
    invalid,
    watch: { updates, cursor },
    afterDispose,
  });
  process.exit(0);
} catch (error) {
  const kind = error instanceof LunaSdkError ? error.kind : 'driver';
  emit({ event: 'error', kind, message: String(error instanceof Error ? error.message : error) });
  process.exit(1);
}
