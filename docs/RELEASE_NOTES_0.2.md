# Lios 0.2.0

Lios 0.2 is a breaking CLI release centered on explicit Space paths and rsync-style transfers.

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
- Upgraded shared configuration to schema v2. The first migration backs up v1 as
  `config.yaml.v1.bak` and reports the explicit `space add` command required to register the old
  Repository Address.
- Recovery Key verification now covers every registered Space before an imported key can replace
  the current key.

Remote deletion remains logical: Lios removes Catalog references but cannot guarantee that
ModelScope storage capacity is reclaimed.
