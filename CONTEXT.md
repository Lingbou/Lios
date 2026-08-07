# Lios

Lios presents encrypted logical drives backed by ModelScope Dataset Repositories. This glossary distinguishes the remote storage container from the encrypted drive that Lios places inside it.

## ModelScope storage

**Endpoint**:
The ModelScope service location on which a Dataset Repository exists.
_Avoid_: Account, Space

**Namespace**:
The ModelScope user or organization that owns a Dataset Repository.
_Avoid_: Account, Space

**Dataset Name**:
The name of a Dataset Repository within a Namespace. It names the repository, not a directory or an individual dataset file.
_Avoid_: Space Name, folder

**Repository Address**:
The combination of Endpoint, Namespace, and Dataset Name that identifies one ModelScope Dataset Repository.
_Avoid_: Space ID, Space Name

**Dataset Repository**:
A ModelScope-hosted repository that can contain Lios metadata and encrypted content. A Dataset Repository is not a Lios Space until it contains a valid Lios Catalog.
_Avoid_: Space when the repository has not been initialized by Lios

## Encrypted drive

**Lios Space**:
An encrypted logical drive stored in one Dataset Repository and described by a valid Lios Catalog. One Lios Space currently occupies one Dataset Repository.
_Avoid_: Dataset Repository when no valid Lios Catalog exists

**Space Name**:
A 2–32-character lowercase, local alias in one Lios Home that maps to exactly one Repository Address. CLI operands use the alias as `name:` or `name:/absolute/catalog/path`. The alias is not the remote identity and may be renamed without changing the Lios Space.
_Avoid_: Repository Address, Dataset Name, remote ID

**Catalog**:
The encrypted description of a Lios Space's logical directory tree and its references to encrypted file content.
_Avoid_: Dataset, local filesystem

**Catalog Root**:
The top-level directory of a Lios Space, addressed as `/` within that Space.
_Avoid_: Space

**Catalog Node**:
A file or directory entry in a Catalog with its own stable identity and parent relationship.
_Avoid_: Content Object

**Content Object**:
The encrypted file content referenced by a file Catalog Node. It is storage content, not a user-visible directory entry.
_Avoid_: Catalog Node, Space

## Explicit location

**Space Path**:
An explicit CLI location made from a Space Name and an absolute Catalog path, such as `photos:` for the Catalog Root or `photos:/docs` for a subtree. Lios has no Active Repository or Active Space; each Catalog and transfer request carries its Space Name.
_Avoid_: node ID as a user-facing path, remote relative path

**Local Location**:
A normal operating-system path used on the local side of `cp` or `sync`. A local name that could look like a Space Path is disambiguated explicitly, for example `./photos:`.
_Avoid_: Catalog path

## Security and operations

**Recovery Key**:
The secret key material used to encrypt and decrypt Lios Catalogs and file content.
_Avoid_: ModelScope Token, password

**ModelScope Token**:
The credential used to authenticate remote ModelScope requests. It does not encrypt or decrypt a Lios Space.
_Avoid_: Recovery Key

**Durable Task**:
A persisted Lios operation, such as a transfer or verification, that can be inspected or resumed after interruption.
_Avoid_: transient request

**Transfer Plan**:
The confirmed, durable list of create, update, skip, delete, and type-change actions for one command that changes a Catalog or Content Object. It records source and destination baselines and is never silently recomputed after confirmation. Authentication, Recovery Key, and Space registry changes are atomic configuration mutations, not Transfer Plans.
_Avoid_: best-effort rescan during apply, configuration mutation plan

**Task Worker**:
The single `lios-worker` process for one Lios Home. CLI and Desktop submit durable tasks to the same worker and observe the same lifecycle.
_Avoid_: Desktop-only runner, foreground-only task
