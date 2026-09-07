# Shared Storage: High-Level Flow

This document explains, at a very high level, how Noland shared storage works and which tools/components are involved.

Shared storage lets a Vast GPU instance save and restore selected app/game state through a cloud-backed repository. The local Noland app controls the flow, but the heavy work happens on the rented Vast instance through the `state-agent`.

## Big Picture

```mermaid
flowchart TD
    User[User] --> App[Noland Desktop App]

    subgraph LocalDevice[Local Device: macOS / Windows / Linux]
        App
        StateJson[state.json<br/>settings, provider refs,<br/>instance metadata]
    end

    subgraph VastInstance[Vast GPU Instance]
        RemoteExec[RemoteExec over SSH]
        Agent[state-agent]
        AppFiles[Apps / game saves / selected paths]
    end

    subgraph CloudStorage[Shared Cloud Storage]
        Provider[Cloud Provider<br/>Google Drive / rclone-backed remote]
        Manifest[Manifest / Catalog]
        Packs[Encrypted content packs]
        Latest[LATEST / COMMITTED pointers]
    end

    App -->|backend command| RemoteExec
    RemoteExec -->|ensure agent exists| Agent
    Agent -->|scan| AppFiles
    Agent -->|pack + encrypt| Packs
    Agent -->|write metadata| Manifest
    Agent -->|update active version| Latest
    Packs --> Provider
    Manifest --> Provider
    Latest --> Provider

    Provider -->|restore reads snapshot| Agent
    Agent -->|decrypt + verify + restore| AppFiles
    Agent -->|result/status| App
```

## Backup / Sync Flow

```mermaid
sequenceDiagram
    participant User
    participant UI as Noland UI
    participant Backend as Tauri Backend
    participant SSH as RemoteExec / SSH / SCP
    participant Agent as state-agent on Vast
    participant Cloud as Cloud Storage Provider

    User->>UI: Click backup / sync
    UI->>Backend: Invoke shared storage command
    Backend->>SSH: Connect to Vast instance
    Backend->>SSH: ensure_state_agent()
    SSH->>Agent: Probe API version or install/start agent
    Backend->>Agent: call_agent_raw over remote agent API
    Agent->>Agent: Scan selected app/files
    Agent->>Agent: Build manifest/catalog
    Agent->>Agent: Pack immutable content
    Agent->>Agent: Encrypt with shared repository key
    Agent->>Cloud: Upload encrypted packs + manifest
    Agent->>Cloud: Update LATEST / COMMITTED pointers
    Agent-->>Backend: Return backup status
    Backend-->>UI: Show success/failure
```

## Restore Flow

```mermaid
sequenceDiagram
    participant User
    participant UI as Noland UI
    participant Backend as Tauri Backend
    participant SSH as RemoteExec / SSH / SCP
    participant Agent as state-agent on Vast
    participant Cloud as Cloud Storage Provider

    User->>UI: Choose restore / selected items
    UI->>Backend: Invoke restore command
    Backend->>SSH: Connect to Vast instance
    Backend->>SSH: ensure_state_agent()
    SSH->>Agent: Probe API version or install/start agent
    Backend->>Agent: Request available snapshots/items
    Agent->>Cloud: Read LATEST / manifests / catalogs
    Cloud-->>Agent: Return snapshot metadata
    Agent-->>Backend: Return restorable items
    Backend-->>UI: Show items to restore
    User->>UI: Confirm restore
    UI->>Backend: Restore selected items
    Backend->>Agent: Restore request
    Agent->>Cloud: Download encrypted packs
    Agent->>Agent: Decrypt and verify content
    Agent->>Agent: Restore selected paths on Vast instance
    Agent->>Agent: Refresh local index
    Agent-->>Backend: Return restore status
    Backend-->>UI: Show success/failure
```

## Tools and Components Used

| Area | Tool / Component | What it does |
| --- | --- | --- |
| Local app | React UI | Lets the user choose backup, sync, restore, provider, and selected items. |
| Local app | Tauri backend commands | Receives UI requests and coordinates shared-storage operations. |
| Local state | `state.json` | Stores settings, provider references, instance metadata, and shared-storage configuration. It does **not** store the full app/game data. |
| Remote access | `RemoteExec` | Backend abstraction for running commands on the Vast instance. |
| Remote access | SSH | Connects from the local app/backend to the Vast instance. |
| Remote upload | SCP | Uploads the bundled `state-agent` source/archive when the agent needs to be installed or refreshed. |
| Agent setup | `ensure_state_agent()` | Checks whether the remote agent is available and compatible; if not, uploads/bootstrap-starts it. |
| Agent packaging | Rust `flate2` + `tar` | Packs bundled `state-agent` source into a `.tar.gz` without depending on platform shell `tar`. |
| Remote service | `systemd` / socket service | Runs the `state-agent` on the Vast instance so the backend can call it reliably. |
| Agent API | `call_agent_raw` | Sends raw requests from the Tauri backend to the remote `state-agent`. |
| State logic | `state-agent` | Scans files, indexes app state, creates manifests, packs content, encrypts data, uploads/downloads, and restores selected paths. |
| Storage adapter | `rclone` | Talks to the selected cloud provider using static or OAuth-backed profiles. |
| Cloud provider | Google Drive / other rclone-backed remotes | Stores encrypted packs, manifests, catalogs, and latest snapshot pointers. |
| Repository data | Manifest / catalog | Describes what files/items exist in a snapshot and how to restore them. |
| Repository data | CAS/content packs | Immutable packed file content, encrypted before upload. |
| Repository data | `LATEST` / `COMMITTED` | Pointers that mark the newest safe shared-storage state. |
| Security | App-wide repository key | Encrypts/decrypts shared-storage content. This key is app-wide and should not be deleted when a provider is disconnected. |

## What Lives Where

```mermaid
flowchart LR
    Local[state.json on local machine<br/>preferences, provider refs,<br/>active instance info] --> Control[Controls shared storage]

    Remote[Vast instance<br/>actual app/game files] --> Agent[state-agent scans/restores]

    Agent --> Cloud[Cloud provider<br/>encrypted shared repository]

    Cloud --> Repo[Manifests, catalogs,<br/>encrypted packs,<br/>LATEST/COMMITTED]
```

### Local machine

- Runs the Noland desktop app.
- Stores `state.json` with configuration and references.
- Initiates backup/sync/restore commands.
- Does not store the full shared app/game data unless that data also exists locally for another reason.

### Vast instance

- Contains the live app/game files being backed up or restored.
- Runs `state-agent`.
- Performs scanning, packing, encryption, download, verification, and restore.

### Cloud provider

- Stores the shared repository.
- Stores encrypted data only.
- Can be backed by Google Drive or another `rclone`-supported provider/profile.

## Key Design Rules

1. **The local app orchestrates; the remote agent does the file work.**
2. **Cloud storage stores encrypted snapshots, not plain app files.**
3. **`state.json` stores references/settings, not the full shared-storage payload.**
4. **The repository encryption key is app-wide and must survive provider disconnects.**
5. **Restore is manifest-driven:** the agent reads snapshot metadata, downloads required packs, verifies them, then restores selected paths.
6. **The state-agent is bootstrapped automatically:** if the Vast instance does not have a compatible agent running, the backend uploads and starts one.

## Simplified Mental Model

```text
Noland App
  -> connects to Vast instance
  -> makes sure state-agent is running
  -> asks state-agent to backup or restore

state-agent
  -> scans app/game files
  -> creates encrypted snapshots
  -> uses rclone to upload/download from cloud storage

cloud storage
  -> stores encrypted packs and manifests
  -> lets a future Vast instance restore the same state
```

---

# Deeper Technical View

The shared-storage system has more complexity than a normal upload/download flow because it crosses several trust and runtime boundaries:

1. Local desktop app runtime.
2. Local persisted app state.
3. SSH/SCP transport into a rented machine.
4. A remote service that may or may not already be installed.
5. Cloud provider configuration and auth.
6. Encrypted, content-addressed repository data.
7. Restore semantics for live app/game folders.

## Technical Architecture

```mermaid
flowchart TD
    subgraph Desktop[Desktop Client]
        UI[React UI]
        Store[Frontend Store]
        Commands[Tauri Commands]
        StateStore[state_store.rs<br/>state.json]
        Runtime[agent_runtime.rs]
        Remote[RemoteExec]
    end

    subgraph Transport[Transport Layer]
        SSH[SSH command exec]
        SCP[SCP file upload]
        SocketTunnel[Remote agent API call<br/>Unix socket / local remote command]
    end

    subgraph Instance[Vast Instance]
        Bootstrap[Bootstrap script / install step]
        Systemd[systemd service/socket]
        Agent[state-agent]
        Scanner[Scanner / indexer]
        Packer[Packer / CAS writer]
        Crypto[Encrypt / decrypt]
        Rclone[rclone adapter]
        LivePaths[Live app/game paths]
    end

    subgraph Cloud[Cloud Repository]
        Objects[Encrypted content objects]
        Manifests[Manifests]
        Catalogs[Catalogs / indexes]
        Pointers[LATEST / COMMITTED]
    end

    UI --> Store
    Store --> Commands
    Commands --> StateStore
    Commands --> Runtime
    Runtime --> Remote
    Remote --> SSH
    Remote --> SCP
    Remote --> SocketTunnel

    SSH --> Bootstrap
    SCP --> Bootstrap
    Bootstrap --> Systemd
    Systemd --> Agent
    SocketTunnel --> Agent

    Agent --> Scanner
    Scanner --> LivePaths
    Scanner --> Packer
    Packer --> Crypto
    Crypto --> Rclone
    Rclone --> Objects
    Rclone --> Manifests
    Rclone --> Catalogs
    Rclone --> Pointers

    Objects --> Rclone
    Manifests --> Rclone
    Catalogs --> Rclone
    Pointers --> Rclone
    Rclone --> Crypto
    Crypto --> Agent
    Agent --> LivePaths
```

## Bootstrap Complexity: `ensure_state_agent()`

Before any shared-storage operation can run, the backend needs a working `state-agent` on the Vast instance. The instance is rented/ephemeral, so the app cannot assume the service is already there.

```mermaid
flowchart TD
    Start[Shared storage command starts] --> HasInstance{Active Vast instance?}
    HasInstance -->|No| FailNoInstance[Fail: no remote target]
    HasInstance -->|Yes| Probe[Probe remote agent API/version]

    Probe --> Compatible{Agent running and compatible?}
    Compatible -->|Yes| UseExisting[Use existing agent]
    Compatible -->|No| Locate[Locate bundled state-agent source]

    Locate --> Found{Source found?}
    Found -->|No| FailBundle[Fail: bundled source missing]
    Found -->|Yes| Pack[Pack source as tar.gz<br/>Rust flate2 + tar]

    Pack --> Upload[Upload archive with SCP]
    Upload --> Bootstrap[Run remote bootstrap over SSH]
    Bootstrap --> InstallDeps[Install/build/start agent]
    InstallDeps --> Service[Register/start systemd service/socket]
    Service --> Reprobe[Probe API/version again]

    Reprobe --> Ready{Ready?}
    Ready -->|No| FailStart[Fail with bootstrap logs]
    Ready -->|Yes| UseNew[Use newly started agent]

    UseExisting --> Call[Run backup/restore request]
    UseNew --> Call
```

### Why this is complex

- The Vast instance may be fresh, partially configured, rebooted, or replaced.
- The remote user, paths, package manager state, and service state may vary.
- The desktop app must work on macOS, Windows, and Linux, so it cannot rely on local shell tools like `tar`.
- The app must locate the bundled `state-agent` source from different Tauri resource layouts.
- The backend needs a version/API probe so old remote agents are not used accidentally.

## Backup Internals

A backup/sync is not just “upload this folder”. It is closer to building a small encrypted repository snapshot.

```mermaid
flowchart TD
    Request[Backup request from UI/backend] --> Resolve[Resolve profile, repository, key, selected app/items]
    Resolve --> Discover[Discover selected paths on Vast instance]
    Discover --> Scan[Scan files/directories]
    Scan --> Classify[Classify app sessions / save paths / metadata]
    Classify --> Hash[Hash content]
    Hash --> Dedupe[Deduplicate unchanged content]
    Dedupe --> Pack[Write immutable content packs]
    Pack --> Encrypt[Encrypt packs with repository key]
    Encrypt --> UploadPacks[Upload packs through rclone]
    UploadPacks --> WriteManifest[Write manifest/catalog]
    WriteManifest --> Commit[Move/update COMMITTED pointer]
    Commit --> Latest[Update LATEST pointer]
    Latest --> Result[Return summary to app]
```

### Important details

- **Scanning:** the agent walks selected paths and records file metadata.
- **Classification:** paths may represent different apps, launchers, save folders, or session data.
- **Hashing/CAS:** content can be addressed by hash so unchanged files do not need to be duplicated.
- **Packing:** many files can be packed into fewer repository objects for efficiency.
- **Encryption:** data is encrypted before it is stored in the cloud provider.
- **Manifest/catalog:** metadata describes what exists in the snapshot and how to restore it.
- **Pointers:** `COMMITTED`/`LATEST` are updated last so an incomplete upload does not become the active snapshot.

## Restore Internals

Restore needs to be careful because it writes back into live folders on the Vast instance.

```mermaid
flowchart TD
    Start[Restore request] --> ReadLatest[Read LATEST pointer]
    ReadLatest --> FetchManifest[Fetch manifest/catalog]
    FetchManifest --> Select[Resolve selected items/paths]
    Select --> Plan[Build restore plan]
    Plan --> CheckLocal[Inspect current local remote files]
    CheckLocal --> Conflicts{Conflicts or risky overwrite?}
    Conflicts -->|Yes| Policy[Apply restore policy<br/>overwrite / skip / selective]
    Conflicts -->|No| Download[Download required packs]
    Policy --> Download
    Download --> Decrypt[Decrypt packs]
    Decrypt --> Verify[Verify hashes/manifest]
    Verify --> Stage[Stage restored files]
    Stage --> Apply[Move/copy into final paths]
    Apply --> Refresh[Refresh index]
    Refresh --> Done[Return restore result]
```

### Restore risks the system must handle

- The current Vast instance may already have local changes.
- Some files may be locked or actively used by the app/game.
- A restore may be partial if the user selected only certain items.
- Downloaded content must be verified before replacing files.
- The system should avoid making a partially downloaded or corrupted snapshot look successful.

## Repository Layout Concept

The exact object names can evolve, but conceptually the cloud repository looks like this:

```text
shared-storage-repository/
  LATEST                    # points to newest visible snapshot
  COMMITTED                 # points to newest fully committed write

  manifests/
    snapshot-a.json.enc     # encrypted snapshot metadata
    snapshot-b.json.enc

  catalogs/
    catalog-a.json.enc      # encrypted searchable/index metadata
    catalog-b.json.enc

  packs/
    ab/
      abcd1234....pack.enc  # encrypted immutable content pack
    ef/
      ef987654....pack.enc

  profiles-or-metadata/
    provider-safe metadata  # non-secret or encrypted metadata only
```

## Consistency Model

Shared storage should behave like an eventually consistent encrypted snapshot repository.

```mermaid
stateDiagram-v2
    [*] --> Preparing
    Preparing --> UploadingPacks
    UploadingPacks --> WritingManifest
    WritingManifest --> Committing
    Committing --> Published

    Preparing --> Failed
    UploadingPacks --> Failed
    WritingManifest --> Failed
    Committing --> Failed

    Failed --> [*]
    Published --> [*]
```

### Commit ordering

To avoid exposing broken snapshots:

1. Upload content packs first.
2. Upload manifest/catalog after the packs exist.
3. Update `COMMITTED` after all required data is present.
4. Update `LATEST` only when the snapshot is safe to show/restore.

If the process fails before `LATEST` is updated, the previous snapshot remains the visible restore target.

## Security Boundaries

```mermaid
flowchart LR
    subgraph TrustedLocal[Trusted Local App Data]
        Key[Repository encryption key]
        Config[Provider refs/settings]
    end

    subgraph RemoteInstance[Remote Vast Instance]
        Agent[state-agent uses key during operation]
        Plain[Plain app files exist on instance]
    end

    subgraph Provider[Cloud Provider]
        Enc[Encrypted packs/manifests only]
    end

    Key --> Agent
    Config --> Agent
    Plain --> Agent
    Agent --> Enc
    Enc --> Agent
    Agent --> Plain
```

### Security rules

- The cloud provider should only receive encrypted repository objects.
- The repository encryption key is app-wide and must survive provider disconnect/reconnect.
- Disconnecting Google Drive or another provider should remove provider linkage, not destroy the key needed to read existing backups.
- Secrets should not be written into diagnostic logs or GitHub issue bodies.
- Provider OAuth/static profiles should be treated separately from repository encryption.

## Failure Modes and What They Mean

| Failure | Likely layer | Meaning |
| --- | --- | --- |
| Cannot SSH to instance | Transport | Vast instance is not reachable, not ready, or credentials/network path are wrong. |
| SCP upload fails | Transport/bootstrap | The backend cannot upload the `state-agent` bundle. |
| Bundled agent source missing | Local packaging | Tauri resource layout or bundled resources are wrong. |
| Agent version probe fails | Remote service | Agent is not running, socket is missing, or API is incompatible. |
| `rclone` config/auth fails | Provider adapter | Cloud provider profile is missing, expired, or invalid. |
| Upload fails before `LATEST` | Repository write | New snapshot should not become visible; previous snapshot remains active. |
| Manifest exists but packs missing | Repository integrity | Snapshot is incomplete/corrupt and should fail verification. |
| Decryption fails | Security/key | Wrong repository key, corrupted object, or incompatible encryption format. |
| Restore verification fails | Integrity | Downloaded content does not match manifest/hash. |
| Restore apply fails | Remote filesystem | Permission issue, missing path, locked file, or disk problem on Vast instance. |

## Why `state.json` Is Not the Shared Storage Repository

`state.json` is local app state. It should contain things like:

- selected provider/profile references;
- active/recent Vast instance IDs;
- shared-storage settings;
- orchestration state;
- UI-visible status and preferences.

It should **not** contain the full app/game backup payload.

The actual backup payload is stored remotely as encrypted repository objects:

```text
state.json = local control plane state
cloud repository = encrypted shared data plane
Vast instance = live filesystem being backed up/restored
```

## End-to-End Technical Flow

```mermaid
sequenceDiagram
    participant UI as React UI
    participant Cmd as Tauri Command
    participant Store as state_store.rs
    participant Runtime as agent_runtime.rs
    participant SSH as SSH/SCP RemoteExec
    participant Agent as state-agent
    participant Rclone as rclone
    participant Cloud as Cloud Provider

    UI->>Cmd: User requests backup/restore
    Cmd->>Store: Load shared-storage config from state.json
    Cmd->>Runtime: ensure_state_agent(instance)
    Runtime->>SSH: Probe agent version

    alt Agent missing or incompatible
        Runtime->>Runtime: Locate bundled state-agent source
        Runtime->>Runtime: Pack source with flate2 + tar
        Runtime->>SSH: SCP archive to Vast instance
        Runtime->>SSH: Run bootstrap/start service
        Runtime->>SSH: Probe agent version again
    end

    Cmd->>Agent: call_agent_raw(request)

    alt Backup / sync
        Agent->>Agent: Scan + classify selected paths
        Agent->>Agent: Hash + dedupe + pack
        Agent->>Agent: Encrypt repository objects
        Agent->>Rclone: Upload packs/manifests/pointers
        Rclone->>Cloud: Write encrypted objects
    else Restore
        Agent->>Rclone: Read LATEST + manifest
        Rclone->>Cloud: Download required encrypted objects
        Agent->>Agent: Decrypt + verify
        Agent->>Agent: Stage + apply selected files
    end

    Agent-->>Cmd: Operation result
    Cmd->>Store: Persist status/settings if needed
    Cmd-->>UI: Display result
```

## Practical Debugging Map

When shared storage breaks, debug by layer:

```mermaid
flowchart TD
    Problem[Shared storage failed] --> Local{Local config OK?}
    Local -->|No| FixState[Check state.json/provider refs]
    Local -->|Yes| Reach{Can reach Vast over SSH?}
    Reach -->|No| FixSSH[Check instance status, SSH key, network]
    Reach -->|Yes| AgentReady{Agent healthy?}
    AgentReady -->|No| FixAgent[Check ensure_state_agent/bootstrap/systemd logs]
    AgentReady -->|Yes| ProviderOK{Provider auth OK?}
    ProviderOK -->|No| FixProvider[Check rclone profile/OAuth/static config]
    ProviderOK -->|Yes| RepoOK{Repo objects valid?}
    RepoOK -->|No| FixRepo[Check manifests/packs/LATEST/COMMITTED]
    RepoOK -->|Yes| FilesOK{Filesystem apply OK?}
    FilesOK -->|No| FixFiles[Check permissions/disk/locked paths]
    FilesOK -->|Yes| Investigate[Inspect agent logs and operation summary]
```

## Main Complexity Summary

The hard parts are not the cloud upload itself. The hard parts are:

- bootstrapping a compatible agent onto an ephemeral rented machine;
- keeping the local app cross-platform while controlling a Linux remote host;
- making writes commit-safe so broken snapshots are not published;
- encrypting data independently from the cloud provider;
- preserving the repository key across provider disconnects;
- restoring only selected files without damaging live remote state;
- reporting enough diagnostics for users to understand which layer failed.
