import type { KeyStore } from "@happyview/oauth-client";

const DEFAULT_DB = "@happyview/oauth-client-browser(keys)";
const STORE = "keys";

/** What a record holds: the pair, and when it was stored. */
type Entry = { pair: CryptoKeyPair; createdAt: number };

/**
 * A key store on IndexedDB, which keeps a `CryptoKey` as the object itself.
 * A non-extractable key stays non-extractable across page loads: the page can
 * sign with it, but can't read it.
 */
export class IndexedDbKeyStore implements KeyStore {
  private db: Promise<IDBDatabase> | undefined;

  constructor(
    private readonly name: string = DEFAULT_DB,
    private readonly factory: IDBFactory = globalThis.indexedDB,
  ) {}

  /** Whether this browser has IndexedDB at all. */
  static isAvailable(): boolean {
    return typeof indexedDB !== "undefined";
  }

  private open(): Promise<IDBDatabase> {
    this.db ??= new Promise((resolve, reject) => {
      const request = this.factory.open(this.name, 1);
      request.onupgradeneeded = () => {
        request.result.createObjectStore(STORE);
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    return this.db;
  }

  private async run<T>(
    mode: IDBTransactionMode,
    action: (store: IDBObjectStore) => IDBRequest<T>,
  ): Promise<T> {
    const db = await this.open();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, mode);
      const request = action(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(request.result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  }

  async get(id: string): Promise<CryptoKeyPair | null> {
    const entry = (await this.run("readonly", (s) => s.get(id))) as Entry | undefined;
    return entry?.pair ?? null;
  }

  async set(id: string, pair: CryptoKeyPair): Promise<void> {
    const entry: Entry = { pair, createdAt: Date.now() };
    await this.run("readwrite", (s) => s.put(entry, id));
  }

  async entries(): Promise<Array<{ id: string; createdAt: number }>> {
    const db = await this.open();
    return new Promise((resolve, reject) => {
      const out: Array<{ id: string; createdAt: number }> = [];
      const tx = db.transaction(STORE, "readonly");
      const cursor = tx.objectStore(STORE).openCursor();
      cursor.onsuccess = () => {
        const c = cursor.result;
        if (!c) return;
        out.push({ id: String(c.key), createdAt: (c.value as Entry).createdAt ?? 0 });
        c.continue();
      };
      tx.oncomplete = () => resolve(out);
      tx.onerror = () => reject(tx.error);
    });
  }

  async delete(id: string): Promise<void> {
    await this.run("readwrite", (s) => s.delete(id));
  }
}
