# Lios CLI 0.2

`lios` is the headless, rsync-style client for encrypted Lios Spaces. Desktop and CLI share the
same `DIR/.lios` configuration, Space registry, Recovery Key, durable task database, and
`lios-worker` process.

Build both installed executables from the repository root:

```bash
cargo build --release --locked -p lios-cli
```

## First use

```bash
lios setup
lios auth login
lios space add photos allen/photos
lios ls photos:
```

`setup` is idempotent. It creates missing local state and a Recovery Key, but never logs in or
replaces an existing key. `auth login` reads the token with hidden terminal input; automation must
use `auth login --token-stdin`. Tokens are not accepted through argv or ordinary environment
variables.

Use global `--home DIR` to isolate state under `DIR/.lios`. Use global `--json` to emit one stable
JSON document and disable interaction and progress output.

## Space paths

A Space is always explicit:

```text
photos:          Space root
photos:/docs     absolute Catalog path
./photos:        local file whose name contains a colon
```

Space Names match `[a-z][a-z0-9_-]{0,31}`. A Space Name is a local alias; the remote identity is
still its ModelScope Repository Address `(endpoint, namespace, dataset)`. There is no Active
Repository or Active Space.

## Commands

```text
lios [--home DIR] [--json] COMMAND

setup
status

auth login [--token-stdin]
auth status
auth logout

key status
key backup DEST
key verify PATH
key import PATH

space create NAME [--namespace N] [--dataset D] [--endpoint URL]
space init NAME OWNER/DATASET [--endpoint URL]
space add NAME OWNER/DATASET [--endpoint URL]
space discover [--endpoint URL]
space list
space show NAME [--remote]
space rename OLD NEW
space remove NAME [--force]

ls SPACE_PATH [--long]
search SPACE_PATH QUERY
mkdir SPACE_PATH... [--parents]
cp SOURCE... DESTINATION
sync SOURCE DESTINATION
mv SOURCE DESTINATION
rm SPACE_PATH... --recursive
verify SPACE: [--full]

task list
task show ID
task wait ID
task pause ID
task resume ID
task retry ID
task cancel ID
task clear ID|--completed|--failed|--all-terminal

worker status
worker stop
```

`cp` copies and `sync` computes a source-wins difference. Direction comes only from operand order;
0.2 supports local-to-Space and Space-to-local transfers, not Space-to-Space. Directory operands
use rsync trailing-slash semantics: `dir` copies the directory itself and `dir/` copies its
contents.

SHA-256 identifies unchanged files. Same-type differences are replaced by default. File/directory
type changes require `--replace-type --yes`. `sync --delete --yes` removes destination-only Catalog
entries inside the selected target subtree; excluded paths are protected from deletion. Remote
deletion removes Catalog references and does not promise ModelScope capacity reclamation.

Symlinks and junctions are rejected. Lios 0.2 does not preserve permissions, ownership, or
modification times.

## Durable worker

Every transfer is persisted before execution. `lios-worker` is started automatically, remains the
sole worker for one Lios Home, recovers tasks after restart, and exits after five idle minutes.
Foreground commands wait for their task; `--detach` returns its task ID immediately.

The first Ctrl-C requests a safe pause and exits with 130. If the task is already publishing an
atomic Catalog transaction, the worker finishes reconciliation before entering a terminal state.
A second Ctrl-C stops only the waiting client immediately.

## Output and exit codes

TTY clients receive stderr progress. Non-TTY clients print only the final result unless
`--progress` is requested. JSON mode writes exactly one envelope to stdout:

```json
{"schema_version":1,"ok":true,"command":"sync","result":{}}
```

Exit codes are stable: `0` success, `2` input, `3` authentication/key/initialization, `4`
network/remote, `5` conflict/lock, `6` corruption/storage, `7` task/internal failure, and `130`
interruption.

## Linux packages

The DEB and RPM metadata install both `/usr/bin/lios` and `/usr/bin/lios-worker`:

```bash
cargo deb -p lios-cli --no-build
cargo generate-rpm -p crates/lios-cli
```
