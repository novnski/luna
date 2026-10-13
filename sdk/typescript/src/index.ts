// @luna/sdk: the typed LUNA client (CORE-05). Types and schemas come from
// the Rust SDK (`generated.ts`); see crates/sdk for the shared contract.

export { API_VERSION, MANIFEST, SDK_VERSION } from './generated.ts';
export type {
  ApiDescription,
  CommandReceipt,
  EngineInfo,
  ErrorKind,
  Operations,
  OperationName,
  TaskCommandRead,
  TaskCommandSnapshot,
  TaskCommandUpdate,
  TaskIntent,
} from './generated.ts';
export { LunaClient, LunaSdkError, TaskCommandWatch, newActor } from './client.ts';
export type { Actor, ConnectOptions, TaskCommandInput } from './client.ts';
export { validate } from './validate.ts';
