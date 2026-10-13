// A minimal external client: it imports only the SDK, never LUNA's engine.
// Rename one task on the local engine and print its receipt:
//
//   node sdk/typescript/examples/rename-task.ts <ipc-port> <task-id> <title>

import { LunaClient, newActor } from '../src/index.ts';

const [port, taskId, title] = process.argv.slice(2);
if (port === undefined || taskId === undefined || title === undefined) {
  console.error('usage: rename-task.ts <ipc-port> <task-id> <title>');
  process.exit(2);
}
const { client, engine } = await LunaClient.connect({ port: Number(port), timeoutMs: 5000 });
try {
  const receipt = await client.renameTask({ actor: newActor(engine.deviceId, 'luna-sdk-example'), taskId, title });
  // A refusal is a receipt: `conflict` with a reason, not an exception.
  console.log(JSON.stringify(receipt, null, 2));
} finally {
  client.close();
}
