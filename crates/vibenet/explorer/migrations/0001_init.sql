-- vibescan indexer schema. Everything derivable from RPC stays in the node;
-- the only persisted state is the cursor + the address-activity join index
-- that plain JSON-RPC cannot serve efficiently.

-- Last block that has been fully processed. We always index up to and
-- including this number; on restart the indexer resumes at next_block.
CREATE TABLE IF NOT EXISTS cursor (
    id                 INTEGER PRIMARY KEY CHECK (id = 0),
    last_indexed_block INTEGER NOT NULL,
    last_indexed_hash  BLOB    NOT NULL,
    updated_at         INTEGER NOT NULL
);

-- Thin block index: just enough to render the home page and paginate by
-- block without round-tripping the RPC for every row. Full block data
-- (transactions, receipts, withdrawals, state root, etc.) is fetched on
-- demand from the node when the detail page is loaded.
CREATE TABLE IF NOT EXISTS blocks (
    number     INTEGER PRIMARY KEY,
    hash       BLOB    NOT NULL UNIQUE,
    timestamp  INTEGER NOT NULL,
    miner      BLOB    NOT NULL,
    tx_count   INTEGER NOT NULL,
    gas_used   INTEGER NOT NULL,
    gas_limit  INTEGER NOT NULL,
    base_fee   BLOB
);

-- Thin tx index for the home feed + reverse lookups. Full tx + receipt are
-- always pulled from the RPC for the detail page; we only keep what we need
-- for listings and to answer "what was the last tx touching this account?".
CREATE TABLE IF NOT EXISTS txs (
    hash        BLOB    PRIMARY KEY,
    block_num   INTEGER NOT NULL,
    tx_index    INTEGER NOT NULL,
    from_addr   BLOB    NOT NULL,
    to_addr     BLOB,
    value       BLOB    NOT NULL,
    status      INTEGER NOT NULL,
    created     BLOB
);
CREATE INDEX IF NOT EXISTS idx_txs_block_num ON txs (block_num DESC, tx_index DESC);

-- The whole reason this service exists: address -> activity feed.
-- `role` values:
--   0 = sender           (top-level tx.from)
--   1 = recipient        (top-level tx.to)
--   2 = creator          (CREATE; paired with tx.created)
--   3 = erc20/721 log from
--   4 = erc20/721 log to
-- `log_index` is NULL for role in (0, 1, 2), otherwise the receipt's log
-- index. The composite primary key prevents double-insertion on restart.
CREATE TABLE IF NOT EXISTS address_activity (
    address    BLOB    NOT NULL,
    block_num  INTEGER NOT NULL,
    tx_index   INTEGER NOT NULL,
    log_index  INTEGER NOT NULL DEFAULT -1,
    tx_hash    BLOB    NOT NULL,
    role       INTEGER NOT NULL,
    token      BLOB,
    PRIMARY KEY (address, block_num, tx_index, log_index, role)
);
CREATE INDEX IF NOT EXISTS idx_activity_addr_block
    ON address_activity (address, block_num DESC, tx_index DESC, log_index DESC);
