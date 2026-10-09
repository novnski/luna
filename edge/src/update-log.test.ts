import { describe, expect, it } from "vitest";
import { LoroDoc } from "loro-crdt";
import { CHUNK_BYTES } from "./blobs";
import { appendUpdateRow, ensureUpdateLog, readUpdateRows } from "./update-log";

/** Minimal SqlStorage fake covering exactly the statements update-log.ts
 * issues, including the ~2MB row cap that motivated chunking. Storage
 * behavior itself (chunking, ordering, migration, the platform cap) is owned
 * by test/workerd/update-log.workerd.test.ts against real DO SQLite; this
 * tier exists only because loro-wasm cannot load in the workerd pool. */
const ROW_CAP = 2 * 1024 * 1024;

class FakeSql {
  rows: { seq: number; bytes: ArrayBuffer; received_at: number; cont: number }[] = [];
  private seq = 0;

  exec(query: string, ...params: unknown[]): Iterable<Record<string, unknown>> {
    if (query.startsWith("CREATE TABLE")) return [];
    if (query.startsWith("ALTER TABLE")) throw new Error("duplicate column name: cont");
    if (query.startsWith("INSERT INTO updates")) {
      const bytes = params[0] as ArrayBuffer;
      if (bytes.byteLength > ROW_CAP) throw new Error("string or blob too big: SQLITE_TOOBIG");
      this.rows.push({
        seq: ++this.seq,
        bytes,
        received_at: params[1] as number,
        cont: (params[2] as number) ?? 0
      });
      return [];
    }
    if (query.startsWith("SELECT bytes, cont FROM updates")) {
      return this.rows.map((r) => ({ bytes: r.bytes, cont: r.cont }));
    }
    if (query.startsWith("DELETE FROM updates")) {
      this.rows = [];
      return [];
    }
    throw new Error(`FakeSql: unhandled query: ${query}`);
  }
}

const asSql = (fake: FakeSql) => fake as unknown as SqlStorage;

const readAll = (fake: FakeSql): Uint8Array[] => [...readUpdateRows(asSql(fake))];

describe("fold path over chunked rows", () => {
  // The session-room fold rides two seams of this module: a cold `ensureDoc`
  // replay imports each reassembled logical update (a chunked whale row group
  // must come back as ONE importable update — Loro rejects a partial blob),
  // and `foldLog` collapses the log into a snapshot with `DELETE FROM
  // updates` before appends resume. Exercise that full cycle with a real
  // LoroDoc, not just byte equality.
  it("replays a chunked whale update, folds it into a snapshot, and keeps appending", () => {
    const sql = new FakeSql();
    ensureUpdateLog(asSql(sql));

    // One commit big enough that its single update spans multiple rows.
    const writer = new LoroDoc();
    writer.getText("t").insert(0, "whale ".repeat(Math.ceil((2 * CHUNK_BYTES) / 6)));
    writer.commit();
    const whale = writer.export({ mode: "update" });
    expect(whale.byteLength).toBeGreaterThan(CHUNK_BYTES);
    appendUpdateRow(asSql(sql), whale, 1);
    expect(sql.rows.length).toBeGreaterThan(1);

    // Cold replay (ensureDoc): every reassembled update must import cleanly.
    const replayed = new LoroDoc();
    for (const update of readUpdateRows(asSql(sql))) replayed.import(update);
    expect(replayed.getText("t").toString() === writer.getText("t").toString()).toBe(true);

    // Fold (foldLog): snapshot the replayed doc, clear the log.
    const snapshot = replayed.export({ mode: "snapshot" });
    sql.exec("DELETE FROM updates");
    expect(readAll(sql).length).toBe(0);

    // Post-fold: new (chunked) appends land against the folded snapshot, and
    // the next cold replay converges to the full doc.
    const before = writer.version();
    writer.getText("t").insert(0, "post-fold ".repeat(Math.ceil(CHUNK_BYTES / 10)));
    writer.commit();
    appendUpdateRow(asSql(sql), writer.export({ mode: "update", from: before }), 2);
    const cold = new LoroDoc();
    cold.import(snapshot);
    for (const update of readUpdateRows(asSql(sql))) cold.import(update);
    expect(cold.getText("t").toString() === writer.getText("t").toString()).toBe(true);
  });
});
