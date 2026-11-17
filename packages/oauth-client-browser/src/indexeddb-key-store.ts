import type { KeyStore } from "@happyview/oauth-client";

const DEFAULT_DB = "@happyview/oauth-client-browser(keys)";
const STORE = "keys";

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
    const pair = await this.run("readonly", (s) => s.get(id));
    return (pair as CryptoKeyPair | undefined) ?? null;
  }

  async set(id: string, pair: CryptoKeyPair): Promise<void> {
    await this.run("readwrite", (s) => s.put(pair, id));
  }

  async delete(id: string): Promise<void> {
    await this.run("readwrite", (s) => s.delete(id));
  }
}
