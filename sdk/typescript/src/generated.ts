// Generated from the luna-sdk Rust types; do not edit. Regenerate with
// `scripts/cargo.sh run --locked -p luna-sdk --features schema --example generate`.
// `scripts/cargo.sh test --locked -p luna-sdk --features schema --test generated`
// fails when this file and the Rust types drift.

export const SDK_VERSION = "0.1.0";
export const API_VERSION = 1;

export type ErrorKind = "offline" | "timeout" | "unsupported" | "incompatible" | "invalidRequest" | "invalidResponse" | "engine" | "unknownOutcome";

export type JsonSchema = boolean | { [keyword: string]: unknown };

export interface OperationEntry {
  name: OperationName;
  method: string;
  id: string;
  version: number;
  kind: string;
  reply: string;
  params: JsonSchema;
  output: JsonSchema;
}

export interface IntentEntry {
  kind: string;
  fences: { taskRevision: boolean; bindingGeneration: boolean; turn: boolean };
}

export interface Manifest {
  sdkVersion: string;
  apiVersion: number;
  operations: OperationEntry[];
  intents: IntentEntry[];
  errorKinds: ErrorKind[];
  frames: { server: JsonSchema; client: JsonSchema };
  /** One `TaskCommandWatch` update, as both SDKs and `luna inspect watch-commands` emit it. */
  watchUpdate: JsonSchema;
  $defs: Record<string, JsonSchema>;
}

/** Operations, intents, frames and every schema they reference. */
export const MANIFEST: Manifest = {
  "$defs": {
    "ApiDescription": {
      "description": "The `DescribeApi` result: what the engine a client is connected to serves.",
      "properties": {
        "apiVersion": {
          "description": "The operation contract version ([`CATALOG_VERSION`]). A client refuses\nan engine with a different API version instead of guessing.",
          "format": "uint16",
          "maximum": 65535,
          "minimum": 0,
          "type": "integer"
        },
        "engineVersion": {
          "description": "The serving engine's build version.",
          "type": "string"
        },
        "operations": {
          "items": {
            "$ref": "#/$defs/OperationRef"
          },
          "type": "array"
        }
      },
      "required": [
        "apiVersion",
        "engineVersion",
        "operations"
      ],
      "type": "object"
    },
    "ClientFrame": {
      "description": "A call ({id, method, params}) or the cancellation of a stream ({id, cancel: true}).",
      "oneOf": [
        {
          "not": {
            "required": [
              "cancel"
            ]
          },
          "required": [
            "method"
          ]
        },
        {
          "not": {
            "required": [
              "method"
            ]
          },
          "required": [
            "cancel"
          ]
        }
      ],
      "properties": {
        "cancel": {
          "const": true
        },
        "id": {
          "minimum": 0,
          "type": "integer"
        },
        "method": {
          "description": "A catalogued operation's wire method name.",
          "type": "string"
        },
        "params": {
          "description": "Operation parameters; omitted or null means none."
        }
      },
      "required": [
        "id"
      ],
      "type": "object"
    },
    "CommandReasonCode": {
      "enum": [
        "commandIdConflict",
        "staleTaskRevision",
        "staleBindingGeneration",
        "staleTurn",
        "queueRevisionConflict",
        "queueClaimed",
        "queueMissing",
        "activeTurnConflict",
        "foreignActive",
        "invalidDestination",
        "incompatibleWorkspace",
        "incompatibleEnvironment",
        "providerAcceptanceUnknown",
        "transactionFailed",
        "unsupportedOperation",
        "operationRejected",
        "operationFailed"
      ],
      "type": "string"
    },
    "CommandReceipt": {
      "properties": {
        "acceptedAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "actorId": {
          "type": "string"
        },
        "clientId": {
          "type": "string"
        },
        "clientSessionId": {
          "type": "string"
        },
        "commandId": {
          "type": "string"
        },
        "committedAtMs": {
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        },
        "correlationId": {
          "description": "Engine/provider correlation used to finalize a pending send only after\nthe provider confirms the matching turn/message boundary.",
          "type": [
            "string",
            "null"
          ]
        },
        "eventSequence": {
          "format": "uint64",
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "executionEnvironment": {
          "type": [
            "string",
            "null"
          ]
        },
        "intent": {
          "type": "string"
        },
        "observedAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "payloadHash": {
          "type": "string"
        },
        "reason": {
          "anyOf": [
            {
              "$ref": "#/$defs/CommandReceiptReason"
            },
            {
              "type": "null"
            }
          ]
        },
        "recoveryAction": {
          "type": [
            "string",
            "null"
          ]
        },
        "snapshot": {
          "$ref": "#/$defs/TaskCommandSnapshot"
        },
        "status": {
          "$ref": "#/$defs/CommandReceiptStatus"
        },
        "taskId": {
          "type": "string"
        }
      },
      "required": [
        "commandId",
        "taskId",
        "intent",
        "payloadHash",
        "status",
        "snapshot",
        "actorId",
        "clientId",
        "clientSessionId",
        "acceptedAtMs",
        "observedAtMs"
      ],
      "type": "object"
    },
    "CommandReceiptReason": {
      "properties": {
        "code": {
          "$ref": "#/$defs/CommandReasonCode"
        },
        "message": {
          "type": "string"
        }
      },
      "required": [
        "code",
        "message"
      ],
      "type": "object"
    },
    "CommandReceiptStatus": {
      "enum": [
        "pending",
        "applied",
        "rejected",
        "conflict",
        "superseded",
        "failed"
      ],
      "type": "string"
    },
    "EmptyParams": {
      "description": "Parameters of an operation that takes none.",
      "type": "object"
    },
    "EngineInfo": {
      "description": "Stable information about the engine runtime reached by a client.",
      "properties": {
        "capabilities": {
          "description": "Supported protocol/document features. Missing on older engines.",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "deviceId": {
          "type": "string"
        },
        "profileId": {
          "description": "Stable opaque identity for the resolved profile, not just this device.\nAbsent on older engines and before an account/local profile is resolved.",
          "type": [
            "string",
            "null"
          ]
        },
        "workspaceScope": {
          "$ref": "#/$defs/WorkspaceScope"
        }
      },
      "required": [
        "deviceId",
        "workspaceScope"
      ],
      "type": "object"
    },
    "ExecuteTaskCommandParams": {
      "description": "The `ExecuteTaskCommand` envelope as the SDK sends it. Absent optional\nfields are omitted, so the engine applies its documented defaults.",
      "properties": {
        "actorId": {
          "type": "string"
        },
        "clientId": {
          "type": "string"
        },
        "clientSessionId": {
          "type": "string"
        },
        "commandId": {
          "description": "Stable idempotency key, minted once before the first send.",
          "type": "string"
        },
        "expectedBindingGeneration": {
          "format": "uint32",
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "expectedTaskRevision": {
          "format": "uint64",
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "expectedTurnId": {
          "type": [
            "string",
            "null"
          ]
        },
        "intent": {
          "$ref": "#/$defs/TaskIntent"
        },
        "issuedAtMs": {
          "description": "Client issue time in Unix milliseconds; omitted, the engine's receive time.",
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        },
        "targetDeviceId": {
          "description": "The task's execution host, when it is not the connected engine.",
          "type": [
            "string",
            "null"
          ]
        },
        "taskId": {
          "type": "string"
        }
      },
      "required": [
        "commandId",
        "actorId",
        "clientId",
        "clientSessionId",
        "taskId",
        "intent"
      ],
      "type": "object"
    },
    "OperationRef": {
      "description": "One served operation, as `DescribeApi` reports it (CORE-05). `kind` and\n`reply` stay strings so an older reader keeps a newer engine's values.",
      "properties": {
        "id": {
          "description": "Stable namespaced operation id (`luna.core/<operation>`).",
          "type": "string"
        },
        "kind": {
          "description": "`query`, `command` or `watch`.",
          "type": "string"
        },
        "method": {
          "description": "Legacy wire method name.",
          "type": "string"
        },
        "reply": {
          "description": "`unary`, `stream` or `streamAfterReady`.",
          "type": "string"
        },
        "version": {
          "format": "uint16",
          "maximum": 65535,
          "minimum": 0,
          "type": "integer"
        }
      },
      "required": [
        "id",
        "version",
        "method",
        "kind",
        "reply"
      ],
      "type": "object"
    },
    "ProjectionCurrent": {
      "description": "A `current` answer: the client's cursor still names this host's state.",
      "properties": {
        "cursor": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "host": {
          "$ref": "#/$defs/ProjectionHost"
        },
        "observedAtMs": {
          "format": "int64",
          "type": "integer"
        }
      },
      "required": [
        "host",
        "cursor",
        "observedAtMs"
      ],
      "type": "object"
    },
    "ProjectionCursor": {
      "description": "Where a projection is in its host's history.",
      "properties": {
        "generation": {
          "description": "Fresh per engine start; revisions compare only within one generation.",
          "type": "string"
        },
        "revision": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        }
      },
      "required": [
        "generation",
        "revision"
      ],
      "type": "object"
    },
    "ProjectionHost": {
      "description": "The host a projection describes. A client checks it against the host it\npaired with before applying anything.",
      "properties": {
        "capabilities": {
          "description": "What this host serves remotely for these projections and tasks, for\nexample `remote-projections-v1`. Task control is absent until a host\nadmits remote commands (RM-04).",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "deviceId": {
          "type": "string"
        },
        "engineVersion": {
          "type": "string"
        },
        "environmentId": {
          "description": "The remote environment (profile) id paired clients target; absent\nwhen this engine has no remote authority store.",
          "type": [
            "string",
            "null"
          ]
        },
        "generation": {
          "type": "string"
        },
        "profileId": {
          "type": [
            "string",
            "null"
          ]
        },
        "workspaceScope": {
          "$ref": "#/$defs/WorkspaceScope"
        }
      },
      "required": [
        "deviceId",
        "workspaceScope",
        "generation",
        "engineVersion",
        "capabilities"
      ],
      "type": "object"
    },
    "ProviderReadiness": {
      "description": "A provider (harness) on this host and whether it can run work.",
      "properties": {
        "enabled": {
          "type": "boolean"
        },
        "id": {
          "type": "string"
        },
        "installed": {
          "type": "boolean"
        },
        "name": {
          "type": "string"
        }
      },
      "required": [
        "id",
        "name",
        "installed",
        "enabled"
      ],
      "type": "object"
    },
    "RemoteHistoryEntry": {
      "description": "One transcript entry, reduced to what a list shows.",
      "properties": {
        "createdAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "entryId": {
          "type": "string"
        },
        "error": {
          "type": "boolean"
        },
        "otherParts": {
          "description": "Parts not shown here (images, reasoning, questions, extra tools).",
          "format": "uint32",
          "minimum": 0,
          "type": "integer"
        },
        "role": {
          "description": "`user`, `assistant` or `system`.",
          "type": "string"
        },
        "status": {
          "description": "`streaming`, `complete` or `aborted`, when the entry has one.",
          "type": [
            "string",
            "null"
          ]
        },
        "text": {
          "description": "The entry's text parts, joined and bounded.",
          "type": "string"
        },
        "textTruncated": {
          "type": "boolean"
        },
        "tools": {
          "description": "The kind of each tool call, in order and bounded.",
          "items": {
            "type": "string"
          },
          "type": "array"
        }
      },
      "required": [
        "entryId",
        "role",
        "createdAtMs",
        "text",
        "textTruncated",
        "tools",
        "otherParts",
        "error"
      ],
      "type": "object"
    },
    "RemoteProject": {
      "description": "One project, by display name only.",
      "properties": {
        "name": {
          "type": "string"
        },
        "projectId": {
          "type": "string"
        }
      },
      "required": [
        "projectId",
        "name"
      ],
      "type": "object"
    },
    "RemoteTaskDelta": {
      "description": "A task's changes from `base` to `cursor`.",
      "properties": {
        "base": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "cursor": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "observedAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "order": {
          "description": "The complete entry order after this delta, when it changed. Always\npresent when an entry was added or removed.",
          "items": {
            "type": "string"
          },
          "type": [
            "array",
            "null"
          ]
        },
        "remove": {
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "task": {
          "anyOf": [
            {
              "$ref": "#/$defs/RemoteTaskRow"
            },
            {
              "type": "null"
            }
          ],
          "description": "The task's row, when it changed."
        },
        "taskRevision": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        },
        "taskSequence": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        },
        "totalEntries": {
          "format": "uint32",
          "minimum": 0,
          "type": "integer"
        },
        "upsert": {
          "items": {
            "$ref": "#/$defs/RemoteHistoryEntry"
          },
          "type": "array"
        }
      },
      "required": [
        "base",
        "cursor",
        "observedAtMs",
        "taskRevision",
        "taskSequence",
        "upsert",
        "remove",
        "totalEntries"
      ],
      "type": "object"
    },
    "RemoteTaskDetail": {
      "description": "One task in detail: the first item of a watch without a current cursor.",
      "properties": {
        "cursor": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "entries": {
          "description": "The newest entries, oldest first.",
          "items": {
            "$ref": "#/$defs/RemoteHistoryEntry"
          },
          "type": "array"
        },
        "host": {
          "$ref": "#/$defs/ProjectionHost"
        },
        "observedAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "task": {
          "$ref": "#/$defs/RemoteTaskRow"
        },
        "taskRevision": {
          "description": "The task's durable command revision and sequence (CORE-05).",
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        },
        "taskSequence": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        },
        "totalEntries": {
          "description": "Every entry in the transcript; more than `entries.len()` when bounded.",
          "format": "uint32",
          "minimum": 0,
          "type": "integer"
        }
      },
      "required": [
        "host",
        "cursor",
        "observedAtMs",
        "task",
        "taskRevision",
        "taskSequence",
        "entries",
        "totalEntries"
      ],
      "type": "object"
    },
    "RemoteTaskItem": {
      "description": "One `WatchRemoteTask` item.",
      "oneOf": [
        {
          "description": "The whole task; replaces anything the client holds for it.",
          "properties": {
            "detail": {
              "$ref": "#/$defs/RemoteTaskDetail"
            },
            "kind": {
              "const": "snapshot",
              "type": "string"
            }
          },
          "required": [
            "kind",
            "detail"
          ],
          "type": "object"
        },
        {
          "description": "The client's cursor is current; nothing was resent.",
          "properties": {
            "current": {
              "$ref": "#/$defs/ProjectionCurrent"
            },
            "kind": {
              "const": "current",
              "type": "string"
            }
          },
          "required": [
            "kind",
            "current"
          ],
          "type": "object"
        },
        {
          "properties": {
            "delta": {
              "$ref": "#/$defs/RemoteTaskDelta"
            },
            "kind": {
              "const": "delta",
              "type": "string"
            }
          },
          "required": [
            "kind",
            "delta"
          ],
          "type": "object"
        },
        {
          "description": "The task is not on this host (deleted, never existed, or its project\nwas removed). The stream ends after it.",
          "properties": {
            "kind": {
              "const": "removed",
              "type": "string"
            },
            "removed": {
              "$ref": "#/$defs/RemovedTask"
            }
          },
          "required": [
            "kind",
            "removed"
          ],
          "type": "object"
        }
      ]
    },
    "RemoteTaskParams": {
      "description": "`WatchRemoteTask` parameters.",
      "properties": {
        "after": {
          "anyOf": [
            {
              "$ref": "#/$defs/ProjectionCursor"
            },
            {
              "type": "null"
            }
          ],
          "description": "The cursor the client holds for this task; omitted, a snapshot."
        },
        "taskId": {
          "type": "string"
        }
      },
      "required": [
        "taskId"
      ],
      "type": "object"
    },
    "RemoteTaskRegistry": {
      "description": "The whole registry: the first item of a watch without a current cursor.",
      "properties": {
        "cursor": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "host": {
          "$ref": "#/$defs/ProjectionHost"
        },
        "observedAtMs": {
          "description": "The host's clock (Unix ms) when this was computed.",
          "format": "int64",
          "type": "integer"
        },
        "projects": {
          "items": {
            "$ref": "#/$defs/RemoteProject"
          },
          "type": "array"
        },
        "providers": {
          "items": {
            "$ref": "#/$defs/ProviderReadiness"
          },
          "type": "array"
        },
        "tasks": {
          "description": "Host order: pinned first, then the most recent activity.",
          "items": {
            "$ref": "#/$defs/RemoteTaskRow"
          },
          "type": "array"
        },
        "totalTasks": {
          "description": "Every task the host would list; more than `tasks.len()` when bounded.",
          "format": "uint32",
          "minimum": 0,
          "type": "integer"
        }
      },
      "required": [
        "host",
        "cursor",
        "observedAtMs",
        "projects",
        "tasks",
        "totalTasks",
        "providers"
      ],
      "type": "object"
    },
    "RemoteTaskRow": {
      "description": "One task's cheap registry row.",
      "properties": {
        "archived": {
          "type": "boolean"
        },
        "createdAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "lastActivityMs": {
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        },
        "pinned": {
          "type": "boolean"
        },
        "preview": {
          "description": "A bounded preview of the last message.",
          "type": [
            "string",
            "null"
          ]
        },
        "projectId": {
          "type": "string"
        },
        "provider": {
          "description": "The task's provider id, when it has one.",
          "type": [
            "string",
            "null"
          ]
        },
        "status": {
          "$ref": "#/$defs/RemoteTaskStatus"
        },
        "taskId": {
          "type": "string"
        },
        "title": {
          "type": [
            "string",
            "null"
          ]
        },
        "unseen": {
          "type": "boolean"
        }
      },
      "required": [
        "taskId",
        "projectId",
        "status",
        "archived",
        "pinned",
        "unseen",
        "createdAtMs"
      ],
      "type": "object"
    },
    "RemoteTaskStatus": {
      "description": "A task's status as the host derives it from its own live runs.",
      "oneOf": [
        {
          "enum": [
            "idle",
            "working",
            "awaitingApproval",
            "awaitingInput",
            "errored"
          ],
          "type": "string"
        },
        {
          "const": "completed",
          "description": "Finished, and not yet seen on any device.",
          "type": "string"
        }
      ]
    },
    "RemoteTasksDelta": {
      "description": "The registry changes from `base` to `cursor`.",
      "properties": {
        "base": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "cursor": {
          "$ref": "#/$defs/ProjectionCursor"
        },
        "observedAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "order": {
          "description": "The complete task order after this delta, when it changed. Always\npresent when a row was added or removed.",
          "items": {
            "type": "string"
          },
          "type": [
            "array",
            "null"
          ]
        },
        "projects": {
          "description": "The complete project list, when it changed.",
          "items": {
            "$ref": "#/$defs/RemoteProject"
          },
          "type": [
            "array",
            "null"
          ]
        },
        "remove": {
          "description": "Rows that left the registry.",
          "items": {
            "type": "string"
          },
          "type": "array"
        },
        "totalTasks": {
          "format": "uint32",
          "minimum": 0,
          "type": "integer"
        },
        "upsert": {
          "description": "New or changed rows.",
          "items": {
            "$ref": "#/$defs/RemoteTaskRow"
          },
          "type": "array"
        }
      },
      "required": [
        "base",
        "cursor",
        "observedAtMs",
        "upsert",
        "remove",
        "totalTasks"
      ],
      "type": "object"
    },
    "RemoteTasksItem": {
      "description": "One `WatchRemoteTasks` item.",
      "oneOf": [
        {
          "description": "The whole registry; replaces anything the client holds.",
          "properties": {
            "kind": {
              "const": "snapshot",
              "type": "string"
            },
            "registry": {
              "$ref": "#/$defs/RemoteTaskRegistry"
            }
          },
          "required": [
            "kind",
            "registry"
          ],
          "type": "object"
        },
        {
          "description": "The client's cursor is current; nothing was resent.",
          "properties": {
            "current": {
              "$ref": "#/$defs/ProjectionCurrent"
            },
            "kind": {
              "const": "current",
              "type": "string"
            }
          },
          "required": [
            "kind",
            "current"
          ],
          "type": "object"
        },
        {
          "properties": {
            "delta": {
              "$ref": "#/$defs/RemoteTasksDelta"
            },
            "kind": {
              "const": "delta",
              "type": "string"
            }
          },
          "required": [
            "kind",
            "delta"
          ],
          "type": "object"
        }
      ]
    },
    "RemoteTasksParams": {
      "description": "`WatchRemoteTasks` parameters.",
      "properties": {
        "after": {
          "anyOf": [
            {
              "$ref": "#/$defs/ProjectionCursor"
            },
            {
              "type": "null"
            }
          ],
          "description": "The cursor the client holds; omitted, the watch starts with a snapshot."
        }
      },
      "type": "object"
    },
    "RemovedTask": {
      "description": "A task this host does not list.",
      "properties": {
        "host": {
          "$ref": "#/$defs/ProjectionHost"
        },
        "observedAtMs": {
          "format": "int64",
          "type": "integer"
        },
        "taskId": {
          "type": "string"
        }
      },
      "required": [
        "host",
        "taskId",
        "observedAtMs"
      ],
      "type": "object"
    },
    "ServerFrame": {
      "description": "Exactly one of ok (present, may be null: success), err (failure), item (stream element) or done: true (stream end).",
      "oneOf": [
        {
          "not": {
            "anyOf": [
              {
                "required": [
                  "err"
                ]
              },
              {
                "required": [
                  "item"
                ]
              },
              {
                "required": [
                  "done"
                ]
              }
            ]
          },
          "required": [
            "ok"
          ]
        },
        {
          "not": {
            "anyOf": [
              {
                "required": [
                  "ok"
                ]
              },
              {
                "required": [
                  "item"
                ]
              },
              {
                "required": [
                  "done"
                ]
              }
            ]
          },
          "required": [
            "err"
          ]
        },
        {
          "not": {
            "anyOf": [
              {
                "required": [
                  "ok"
                ]
              },
              {
                "required": [
                  "err"
                ]
              },
              {
                "required": [
                  "done"
                ]
              }
            ]
          },
          "required": [
            "item"
          ]
        },
        {
          "not": {
            "anyOf": [
              {
                "required": [
                  "ok"
                ]
              },
              {
                "required": [
                  "err"
                ]
              },
              {
                "required": [
                  "item"
                ]
              }
            ]
          },
          "required": [
            "done"
          ]
        }
      ],
      "properties": {
        "done": {
          "const": true
        },
        "err": {
          "description": "Failure text; `unknown method: <m>` and `bad params: <reason>` are stable prefixes.",
          "type": "string"
        },
        "id": {
          "minimum": 0,
          "type": "integer"
        },
        "item": {
          "description": "One stream element."
        },
        "ok": {
          "description": "Success value; null is a valid result."
        }
      },
      "required": [
        "id"
      ],
      "type": "object"
    },
    "TaskCommandEvent": {
      "properties": {
        "commandId": {
          "type": "string"
        },
        "receipt": {
          "$ref": "#/$defs/CommandReceipt"
        },
        "sequence": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        },
        "taskSequence": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        }
      },
      "required": [
        "sequence",
        "taskSequence",
        "commandId",
        "receipt"
      ],
      "type": "object"
    },
    "TaskCommandRead": {
      "properties": {
        "events": {
          "default": [],
          "items": {
            "$ref": "#/$defs/TaskCommandEvent"
          },
          "type": "array"
        },
        "hasMore": {
          "default": false,
          "description": "More durable events exist after `next_task_sequence`.",
          "type": "boolean"
        },
        "nextTaskSequence": {
          "description": "Cursor to pass for the next bounded page. Absent for snapshot-only or\nreload-required responses.",
          "format": "uint64",
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "reloadRequired": {
          "description": "True when the requested cursor is ahead of or incompatible with the\ndurable sequence. Consumers must replace local state with `snapshot`.",
          "type": "boolean"
        },
        "snapshot": {
          "$ref": "#/$defs/TaskCommandSnapshot"
        }
      },
      "required": [
        "snapshot",
        "reloadRequired"
      ],
      "type": "object"
    },
    "TaskCommandReceiptParams": {
      "description": "Parameters of a durable receipt lookup.",
      "properties": {
        "commandId": {
          "type": "string"
        },
        "targetDeviceId": {
          "type": [
            "string",
            "null"
          ]
        }
      },
      "required": [
        "commandId"
      ],
      "type": "object"
    },
    "TaskCommandSnapshot": {
      "properties": {
        "activeTurnId": {
          "type": [
            "string",
            "null"
          ]
        },
        "bindingGeneration": {
          "format": "uint32",
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "bindingId": {
          "type": [
            "string",
            "null"
          ]
        },
        "taskId": {
          "type": "string"
        },
        "taskRevision": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        },
        "taskSequence": {
          "format": "uint64",
          "minimum": 0,
          "type": "integer"
        }
      },
      "required": [
        "taskId",
        "taskRevision",
        "taskSequence"
      ],
      "type": "object"
    },
    "TaskCommandSnapshotParams": {
      "description": "Parameters of a task's command snapshot read or watch.",
      "properties": {
        "afterTaskSequence": {
          "description": "With a cursor, the durable events after it (a bounded page); without,\nthe current snapshot only.",
          "format": "uint64",
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "limit": {
          "description": "Page size; the engine clamps it to 1..=100 (default 50).",
          "format": "uint16",
          "maximum": 65535,
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        },
        "targetDeviceId": {
          "description": "The task's execution host, when it is not the connected engine.",
          "type": [
            "string",
            "null"
          ]
        },
        "taskId": {
          "type": "string"
        }
      },
      "required": [
        "taskId"
      ],
      "type": "object"
    },
    "TaskCommandUpdate": {
      "description": "One watch update; serialized as the TypeScript SDK yields it.",
      "oneOf": [
        {
          "description": "The starting point: the snapshot and, after a requested cursor, every\ndurable event since it.",
          "properties": {
            "kind": {
              "const": "snapshot",
              "type": "string"
            },
            "read": {
              "$ref": "#/$defs/TaskCommandRead"
            }
          },
          "required": [
            "kind",
            "read"
          ],
          "type": "object"
        },
        {
          "description": "One terminal command event, delivered once, in task-sequence order.",
          "properties": {
            "kind": {
              "const": "event",
              "type": "string"
            },
            "receipt": {
              "$ref": "#/$defs/CommandReceipt"
            }
          },
          "required": [
            "kind",
            "receipt"
          ],
          "type": "object"
        },
        {
          "description": "A receipt with no durable position yet (accepted, still pending). The\ncursor does not move.",
          "properties": {
            "kind": {
              "const": "progress",
              "type": "string"
            },
            "receipt": {
              "$ref": "#/$defs/CommandReceipt"
            }
          },
          "required": [
            "kind",
            "receipt"
          ],
          "type": "object"
        },
        {
          "description": "Events after the cursor are no longer replayable: replace local state\nwith this snapshot. The cursor moves to its sequence.",
          "properties": {
            "kind": {
              "const": "reload",
              "type": "string"
            },
            "snapshot": {
              "$ref": "#/$defs/TaskCommandSnapshot"
            }
          },
          "required": [
            "kind",
            "snapshot"
          ],
          "type": "object"
        }
      ]
    },
    "TaskCommandWatchItem": {
      "description": "One `WatchTaskCommands` stream item.",
      "oneOf": [
        {
          "description": "First item: the snapshot and, after a cursor, a bounded event page.",
          "properties": {
            "kind": {
              "const": "snapshot",
              "type": "string"
            },
            "read": {
              "$ref": "#/$defs/TaskCommandRead"
            }
          },
          "required": [
            "kind",
            "read"
          ],
          "type": "object"
        },
        {
          "description": "A receipt recorded for the watched task after the subscription began.\nBoxed for size only: serde and the schema treat it as the receipt.",
          "properties": {
            "kind": {
              "const": "receipt",
              "type": "string"
            },
            "receipt": {
              "$ref": "#/$defs/CommandReceipt"
            }
          },
          "required": [
            "kind",
            "receipt"
          ],
          "type": "object"
        }
      ]
    },
    "TaskIntent": {
      "description": "The published task-command intents. Every other intent the engine accepts\nis first-party and may change without an API version.",
      "oneOf": [
        {
          "description": "Rename a task. Fenced by its revision and provider binding generation.",
          "properties": {
            "kind": {
              "const": "rename",
              "type": "string"
            },
            "title": {
              "type": "string"
            }
          },
          "required": [
            "kind",
            "title"
          ],
          "type": "object"
        }
      ]
    },
    "WorkspaceScope": {
      "description": "The fixed data boundary selected when an engine runtime is assembled.\n\nAuthentication can change while a runtime is alive, but its workspace scope\ncannot. Switching scopes requires assembling a new runtime.",
      "enum": [
        "local",
        "synced",
        "development"
      ],
      "type": "string"
    }
  },
  "apiVersion": 1,
  "errorKinds": [
    "offline",
    "timeout",
    "unsupported",
    "incompatible",
    "invalidRequest",
    "invalidResponse",
    "engine",
    "unknownOutcome"
  ],
  "frames": {
    "client": {
      "$ref": "#/$defs/ClientFrame"
    },
    "server": {
      "$ref": "#/$defs/ServerFrame"
    }
  },
  "intents": [
    {
      "fences": {
        "bindingGeneration": true,
        "taskRevision": true,
        "turn": false
      },
      "kind": "rename"
    }
  ],
  "operations": [
    {
      "id": "luna.core/describe-api",
      "kind": "query",
      "method": "DescribeApi",
      "name": "describeApi",
      "output": {
        "$ref": "#/$defs/ApiDescription"
      },
      "params": {
        "$ref": "#/$defs/EmptyParams"
      },
      "reply": "unary",
      "version": 1
    },
    {
      "id": "luna.core/engine-info",
      "kind": "query",
      "method": "EngineInfo",
      "name": "engineInfo",
      "output": {
        "$ref": "#/$defs/EngineInfo"
      },
      "params": {
        "$ref": "#/$defs/EmptyParams"
      },
      "reply": "unary",
      "version": 1
    },
    {
      "id": "luna.core/get-task-command-snapshot",
      "kind": "query",
      "method": "GetTaskCommandSnapshot",
      "name": "getTaskCommandSnapshot",
      "output": {
        "$ref": "#/$defs/TaskCommandRead"
      },
      "params": {
        "$ref": "#/$defs/TaskCommandSnapshotParams"
      },
      "reply": "unary",
      "version": 1
    },
    {
      "id": "luna.core/get-task-command-receipt",
      "kind": "query",
      "method": "GetTaskCommandReceipt",
      "name": "getTaskCommandReceipt",
      "output": {
        "anyOf": [
          {
            "$ref": "#/$defs/CommandReceipt"
          },
          {
            "type": "null"
          }
        ]
      },
      "params": {
        "$ref": "#/$defs/TaskCommandReceiptParams"
      },
      "reply": "unary",
      "version": 1
    },
    {
      "id": "luna.core/execute-task-command",
      "kind": "command",
      "method": "ExecuteTaskCommand",
      "name": "executeTaskCommand",
      "output": {
        "$ref": "#/$defs/CommandReceipt"
      },
      "params": {
        "$ref": "#/$defs/ExecuteTaskCommandParams"
      },
      "reply": "unary",
      "version": 1
    },
    {
      "id": "luna.core/watch-task-commands",
      "kind": "watch",
      "method": "WatchTaskCommands",
      "name": "watchTaskCommands",
      "output": {
        "$ref": "#/$defs/TaskCommandWatchItem"
      },
      "params": {
        "$ref": "#/$defs/TaskCommandSnapshotParams"
      },
      "reply": "stream",
      "version": 1
    },
    {
      "id": "luna.core/watch-remote-tasks",
      "kind": "watch",
      "method": "WatchRemoteTasks",
      "name": "watchRemoteTasks",
      "output": {
        "$ref": "#/$defs/RemoteTasksItem"
      },
      "params": {
        "$ref": "#/$defs/RemoteTasksParams"
      },
      "reply": "stream",
      "version": 1
    },
    {
      "id": "luna.core/watch-remote-task",
      "kind": "watch",
      "method": "WatchRemoteTask",
      "name": "watchRemoteTask",
      "output": {
        "$ref": "#/$defs/RemoteTaskItem"
      },
      "params": {
        "$ref": "#/$defs/RemoteTaskParams"
      },
      "reply": "stream",
      "version": 1
    }
  ],
  "sdkVersion": "0.1.0",
  "watchUpdate": {
    "$ref": "#/$defs/TaskCommandUpdate"
  }
};

/** The `DescribeApi` result: what the engine a client is connected to serves. */
export type ApiDescription = {
  /** The operation contract version ([`CATALOG_VERSION`]). A client refuses an engine with a different API version instead of guessing. */
  apiVersion: number;
  /** The serving engine's build version. */
  engineVersion: string;
  operations: Array<OperationRef>;
};

/** A call ({id, method, params}) or the cancellation of a stream ({id, cancel: true}). */
export type ClientFrame = {
  cancel?: true;
  id: number;
  /** A catalogued operation's wire method name. */
  method?: string;
  /** Operation parameters; omitted or null means none. */
  params?: unknown;
};

export type CommandReasonCode = "commandIdConflict" | "staleTaskRevision" | "staleBindingGeneration" | "staleTurn" | "queueRevisionConflict" | "queueClaimed" | "queueMissing" | "activeTurnConflict" | "foreignActive" | "invalidDestination" | "incompatibleWorkspace" | "incompatibleEnvironment" | "providerAcceptanceUnknown" | "transactionFailed" | "unsupportedOperation" | "operationRejected" | "operationFailed";

export type CommandReceipt = {
  acceptedAtMs: number;
  actorId: string;
  clientId: string;
  clientSessionId: string;
  commandId: string;
  committedAtMs?: number | null;
  /** Engine/provider correlation used to finalize a pending send only after the provider confirms the matching turn/message boundary. */
  correlationId?: string | null;
  eventSequence?: number | null;
  executionEnvironment?: string | null;
  intent: string;
  observedAtMs: number;
  payloadHash: string;
  reason?: CommandReceiptReason | null;
  recoveryAction?: string | null;
  snapshot: TaskCommandSnapshot;
  status: CommandReceiptStatus;
  taskId: string;
};

export type CommandReceiptReason = {
  code: CommandReasonCode;
  message: string;
};

export type CommandReceiptStatus = "pending" | "applied" | "rejected" | "conflict" | "superseded" | "failed";

/** Parameters of an operation that takes none. */
export type EmptyParams = Record<string, unknown>;

/** Stable information about the engine runtime reached by a client. */
export type EngineInfo = {
  /** Supported protocol/document features. Missing on older engines. */
  capabilities?: Array<string>;
  deviceId: string;
  /** Stable opaque identity for the resolved profile, not just this device. Absent on older engines and before an account/local profile is resolved. */
  profileId?: string | null;
  workspaceScope: WorkspaceScope;
};

/** The `ExecuteTaskCommand` envelope as the SDK sends it. Absent optional fields are omitted, so the engine applies its documented defaults. */
export type ExecuteTaskCommandParams = {
  actorId: string;
  clientId: string;
  clientSessionId: string;
  /** Stable idempotency key, minted once before the first send. */
  commandId: string;
  expectedBindingGeneration?: number | null;
  expectedTaskRevision?: number | null;
  expectedTurnId?: string | null;
  intent: TaskIntent;
  /** Client issue time in Unix milliseconds; omitted, the engine's receive time. */
  issuedAtMs?: number | null;
  /** The task's execution host, when it is not the connected engine. */
  targetDeviceId?: string | null;
  taskId: string;
};

/** One served operation, as `DescribeApi` reports it (CORE-05). `kind` and `reply` stay strings so an older reader keeps a newer engine's values. */
export type OperationRef = {
  /** Stable namespaced operation id (`luna.core/<operation>`). */
  id: string;
  /** `query`, `command` or `watch`. */
  kind: string;
  /** Legacy wire method name. */
  method: string;
  /** `unary`, `stream` or `streamAfterReady`. */
  reply: string;
  version: number;
};

/** A `current` answer: the client's cursor still names this host's state. */
export type ProjectionCurrent = {
  cursor: ProjectionCursor;
  host: ProjectionHost;
  observedAtMs: number;
};

/** Where a projection is in its host's history. */
export type ProjectionCursor = {
  /** Fresh per engine start; revisions compare only within one generation. */
  generation: string;
  revision: number;
};

/** The host a projection describes. A client checks it against the host it paired with before applying anything. */
export type ProjectionHost = {
  /** What this host serves remotely for these projections and tasks, for example `remote-projections-v1`. Task control is absent until a host admits remote commands (RM-04). */
  capabilities: Array<string>;
  deviceId: string;
  engineVersion: string;
  /** The remote environment (profile) id paired clients target; absent when this engine has no remote authority store. */
  environmentId?: string | null;
  generation: string;
  profileId?: string | null;
  workspaceScope: WorkspaceScope;
};

/** A provider (harness) on this host and whether it can run work. */
export type ProviderReadiness = {
  enabled: boolean;
  id: string;
  installed: boolean;
  name: string;
};

/** One transcript entry, reduced to what a list shows. */
export type RemoteHistoryEntry = {
  createdAtMs: number;
  entryId: string;
  error: boolean;
  /** Parts not shown here (images, reasoning, questions, extra tools). */
  otherParts: number;
  /** `user`, `assistant` or `system`. */
  role: string;
  /** `streaming`, `complete` or `aborted`, when the entry has one. */
  status?: string | null;
  /** The entry's text parts, joined and bounded. */
  text: string;
  textTruncated: boolean;
  /** The kind of each tool call, in order and bounded. */
  tools: Array<string>;
};

/** One project, by display name only. */
export type RemoteProject = {
  name: string;
  projectId: string;
};

/** A task's changes from `base` to `cursor`. */
export type RemoteTaskDelta = {
  base: ProjectionCursor;
  cursor: ProjectionCursor;
  observedAtMs: number;
  /** The complete entry order after this delta, when it changed. Always present when an entry was added or removed. */
  order?: Array<string> | null;
  remove: Array<string>;
  /** The task's row, when it changed. */
  task?: RemoteTaskRow | null;
  taskRevision: number;
  taskSequence: number;
  totalEntries: number;
  upsert: Array<RemoteHistoryEntry>;
};

/** One task in detail: the first item of a watch without a current cursor. */
export type RemoteTaskDetail = {
  cursor: ProjectionCursor;
  /** The newest entries, oldest first. */
  entries: Array<RemoteHistoryEntry>;
  host: ProjectionHost;
  observedAtMs: number;
  task: RemoteTaskRow;
  /** The task's durable command revision and sequence (CORE-05). */
  taskRevision: number;
  taskSequence: number;
  /** Every entry in the transcript; more than `entries.len()` when bounded. */
  totalEntries: number;
};

/** One `WatchRemoteTask` item. */
export type RemoteTaskItem = {
  detail: RemoteTaskDetail;
  kind: "snapshot";
} | {
  current: ProjectionCurrent;
  kind: "current";
} | {
  delta: RemoteTaskDelta;
  kind: "delta";
} | {
  kind: "removed";
  removed: RemovedTask;
};

/** `WatchRemoteTask` parameters. */
export type RemoteTaskParams = {
  /** The cursor the client holds for this task; omitted, a snapshot. */
  after?: ProjectionCursor | null;
  taskId: string;
};

/** The whole registry: the first item of a watch without a current cursor. */
export type RemoteTaskRegistry = {
  cursor: ProjectionCursor;
  host: ProjectionHost;
  /** The host's clock (Unix ms) when this was computed. */
  observedAtMs: number;
  projects: Array<RemoteProject>;
  providers: Array<ProviderReadiness>;
  /** Host order: pinned first, then the most recent activity. */
  tasks: Array<RemoteTaskRow>;
  /** Every task the host would list; more than `tasks.len()` when bounded. */
  totalTasks: number;
};

/** One task's cheap registry row. */
export type RemoteTaskRow = {
  archived: boolean;
  createdAtMs: number;
  lastActivityMs?: number | null;
  pinned: boolean;
  /** A bounded preview of the last message. */
  preview?: string | null;
  projectId: string;
  /** The task's provider id, when it has one. */
  provider?: string | null;
  status: RemoteTaskStatus;
  taskId: string;
  title?: string | null;
  unseen: boolean;
};

/** A task's status as the host derives it from its own live runs. */
export type RemoteTaskStatus = "idle" | "working" | "awaitingApproval" | "awaitingInput" | "errored" | "completed";

/** The registry changes from `base` to `cursor`. */
export type RemoteTasksDelta = {
  base: ProjectionCursor;
  cursor: ProjectionCursor;
  observedAtMs: number;
  /** The complete task order after this delta, when it changed. Always present when a row was added or removed. */
  order?: Array<string> | null;
  /** The complete project list, when it changed. */
  projects?: Array<RemoteProject> | null;
  /** Rows that left the registry. */
  remove: Array<string>;
  totalTasks: number;
  /** New or changed rows. */
  upsert: Array<RemoteTaskRow>;
};

/** One `WatchRemoteTasks` item. */
export type RemoteTasksItem = {
  kind: "snapshot";
  registry: RemoteTaskRegistry;
} | {
  current: ProjectionCurrent;
  kind: "current";
} | {
  delta: RemoteTasksDelta;
  kind: "delta";
};

/** `WatchRemoteTasks` parameters. */
export type RemoteTasksParams = {
  /** The cursor the client holds; omitted, the watch starts with a snapshot. */
  after?: ProjectionCursor | null;
};

/** A task this host does not list. */
export type RemovedTask = {
  host: ProjectionHost;
  observedAtMs: number;
  taskId: string;
};

/** Exactly one of ok (present, may be null: success), err (failure), item (stream element) or done: true (stream end). */
export type ServerFrame = {
  done?: true;
  /** Failure text; `unknown method: <m>` and `bad params: <reason>` are stable prefixes. */
  err?: string;
  id: number;
  /** One stream element. */
  item?: unknown;
  /** Success value; null is a valid result. */
  ok?: unknown;
};

export type TaskCommandEvent = {
  commandId: string;
  receipt: CommandReceipt;
  sequence: number;
  taskSequence: number;
};

export type TaskCommandRead = {
  events?: Array<TaskCommandEvent>;
  /** More durable events exist after `next_task_sequence`. */
  hasMore?: boolean;
  /** Cursor to pass for the next bounded page. Absent for snapshot-only or reload-required responses. */
  nextTaskSequence?: number | null;
  /** True when the requested cursor is ahead of or incompatible with the durable sequence. Consumers must replace local state with `snapshot`. */
  reloadRequired: boolean;
  snapshot: TaskCommandSnapshot;
};

/** Parameters of a durable receipt lookup. */
export type TaskCommandReceiptParams = {
  commandId: string;
  targetDeviceId?: string | null;
};

export type TaskCommandSnapshot = {
  activeTurnId?: string | null;
  bindingGeneration?: number | null;
  bindingId?: string | null;
  taskId: string;
  taskRevision: number;
  taskSequence: number;
};

/** Parameters of a task's command snapshot read or watch. */
export type TaskCommandSnapshotParams = {
  /** With a cursor, the durable events after it (a bounded page); without, the current snapshot only. */
  afterTaskSequence?: number | null;
  /** Page size; the engine clamps it to 1..=100 (default 50). */
  limit?: number | null;
  /** The task's execution host, when it is not the connected engine. */
  targetDeviceId?: string | null;
  taskId: string;
};

/** One watch update; serialized as the TypeScript SDK yields it. */
export type TaskCommandUpdate = {
  kind: "snapshot";
  read: TaskCommandRead;
} | {
  kind: "event";
  receipt: CommandReceipt;
} | {
  kind: "progress";
  receipt: CommandReceipt;
} | {
  kind: "reload";
  snapshot: TaskCommandSnapshot;
};

/** One `WatchTaskCommands` stream item. */
export type TaskCommandWatchItem = {
  kind: "snapshot";
  read: TaskCommandRead;
} | {
  kind: "receipt";
  receipt: CommandReceipt;
};

/** The published task-command intents. Every other intent the engine accepts is first-party and may change without an API version. */
export type TaskIntent = {
  kind: "rename";
  title: string;
};

/** The fixed data boundary selected when an engine runtime is assembled.  Authentication can change while a runtime is alive, but its workspace scope cannot. Switching scopes requires assembling a new runtime. */
export type WorkspaceScope = "local" | "synced" | "development";

/** Each operation's parameters and its result (or stream item). */
export interface Operations {
  describeApi: { params: EmptyParams; output: ApiDescription };
  engineInfo: { params: EmptyParams; output: EngineInfo };
  getTaskCommandSnapshot: { params: TaskCommandSnapshotParams; output: TaskCommandRead };
  getTaskCommandReceipt: { params: TaskCommandReceiptParams; output: CommandReceipt | null };
  executeTaskCommand: { params: ExecuteTaskCommandParams; output: CommandReceipt };
  watchTaskCommands: { params: TaskCommandSnapshotParams; output: TaskCommandWatchItem };
  watchRemoteTasks: { params: RemoteTasksParams; output: RemoteTasksItem };
  watchRemoteTask: { params: RemoteTaskParams; output: RemoteTaskItem };
}

export type OperationName = keyof Operations;
