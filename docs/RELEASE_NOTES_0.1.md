# Lios 0.1.0

Lios 0.1 is the first official release of the Desktop and CLI, centered on explicit Space paths
and rsync-style transfers.

- Removed Active Repository and the legacy `upload`, `download`, `delete`, `rename`, `repos`, and
  `space open` command syntax.
- Added the shared Space registry and `name:` / `name:/path` operands.
- Added `cp`, `sync`, `mv`, and recursive `rm`, including rsync trailing-slash behavior, SHA-256
  skips, protected excludes, dry runs, explicit type replacement, and optional destination-only
  deletion.
- Added stable JSON envelopes, stable error codes, and documented exit codes.
- Added durable Copy and Sync plans with source/destination fingerprints, Catalog baselines,
  per-action journals, atomic pull replacement, and Catalog transaction reconciliation.
- Added the portable `lios-worker`. CLI and Desktop use the same task database and execution
  engine; detached tasks survive client exit and workers recover interrupted work.
- Shared configuration now requires schema v3. Pre-release state with another schema must be
  deleted and initialized with `lios setup`; there is no migration path.
- Recovery Key verification now covers every registered Space before an imported key can replace
  the current key.

Remote deletion remains logical: Lios removes Catalog references but cannot guarantee that
ModelScope storage capacity is reclaimed.
