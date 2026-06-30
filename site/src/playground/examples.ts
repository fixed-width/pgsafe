export interface Example {
  id: string;
  label: string;
  sql: string;
  inTransaction?: boolean;
}

/** Curated migrations for the playground dropdown — realistic, short, and each
 *  trips (or clears) a specific hazard. Kept consistent with the rules pages. */
export const EXAMPLES: Example[] = [
  {
    id: "blocking-index",
    label: "Blocking index build (unsafe)",
    sql: "CREATE INDEX idx_users_email ON users (email);",
  },
  {
    id: "concurrent-index",
    label: "Concurrent index build (safe)",
    sql: "CREATE INDEX CONCURRENTLY idx_users_email ON users (email);",
  },
  {
    id: "ignore-directive",
    label: "Ignoring a finding (pgsafe:ignore)",
    sql: [
      "-- pgsafe:ignore add-index-non-concurrent  built in a maintenance window",
      "CREATE INDEX idx_users_email ON users (email);",
    ].join("\n"),
  },
  {
    id: "add-not-null",
    label: "Add a NOT NULL column (unsafe)",
    sql: "ALTER TABLE users ADD COLUMN status text NOT NULL;",
  },
  {
    id: "concurrently-in-txn",
    label: "CONCURRENTLY inside a transaction",
    sql: "CREATE INDEX CONCURRENTLY idx_orders_customer ON orders (customer_id);",
    inTransaction: true,
  },
  {
    id: "multi-hazard",
    label: "A migration with several hazards",
    sql: [
      "ALTER TABLE orders ADD CONSTRAINT fk_customer",
      "  FOREIGN KEY (customer_id) REFERENCES customers (id);",
      "",
      "ALTER TABLE orders ALTER COLUMN total TYPE bigint;",
      "",
      "DROP TABLE legacy_orders;",
    ].join("\n"),
  },
  {
    id: "safe-rewrite",
    label: "The safe rewrite of a foreign key",
    sql: [
      "SET lock_timeout = '5s';",
      "ALTER TABLE orders ADD CONSTRAINT fk_customer",
      "  FOREIGN KEY (customer_id) REFERENCES customers (id) NOT VALID;",
      "ALTER TABLE orders VALIDATE CONSTRAINT fk_customer;",
    ].join("\n"),
  },
  {
    id: "with-timeout",
    label: "Guarded with a lock_timeout (safe)",
    sql: [
      "-- a bounded lock_timeout makes a blocking DDL fail fast instead of",
      "-- piling up the lock queue",
      "SET lock_timeout = '5s';",
      "ALTER TABLE users ADD COLUMN status text;",
    ].join("\n"),
  },
];
