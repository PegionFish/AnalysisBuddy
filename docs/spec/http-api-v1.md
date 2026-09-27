# AnalysisBuddy HTTP API v1

> Status: **Frozen** — 2026-09-28 (contract freeze; master plan Wave 0 task T0.2, `docs/audit-and-dev-master-plan-2026-09-27.md`). Any change after the freeze — including from tasks implementing this contract — MUST go through a **CCR** (contract change request): one paragraph stating the change and its blast radius, reviewed and merged serially by the orchestrator (master plan vol-2 §2.5.3). Before the freeze this file was the Active M2 server-mode contract.
>
> This English specification documents the REST + SSE surface exposed by `core/ab-server`, the headless HTTP service edition of AnalysisBuddy, and — as of the freeze — the **service form** built on top of it: an auth gateway spawning one exclusive ab-server instance per session (§8–§10, Appendix A). Response bodies reuse the desktop command DTOs field-for-field; the fact source for those shapes is `core/ab-engine/src/commands/*` and the desktop contract `AnalysisBuddy-devdocs/deep-dive/ipc-ui.md` §1.0. This file introduces no DTO field that is absent there.
>
> Keywords: MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, MAY — per RFC 2119. "Server" means the `ab-server` process; "engine" means the embedded `ab-engine` headless core; "plugin" means a plugin subprocess managed by the engine.

## 1. Transport

| Item | Value |
|------|-------|
| Protocol | HTTP/1.1 (axum); keep-alive supported. |
| Base path | `/api/v1` (all endpoints below are relative to it). |
| Encoding | Request and response bodies are UTF-8 JSON (`application/json`), except multipart uploads (`multipart/form-data`) and the SSE stream (`text/event-stream`). |
| Request body limit | **64 MB** (uploads need headroom for plugin ZIPs). Larger bodies are rejected by the transport layer before routing. |
| Numbering | Integers are JSON numbers (`i64` for timestamps, `u64` for sizes); `f64` for confidences. |
| Auth | Optional `Authorization: Bearer <token>` — see §7.2. In the service form the gateway injects the per-instance token server-side (§8.1). |
| Session | Service form only: the `ab_sid` cookie issued by the gateway is the session capability — endpoints and semantics in §8. |
| Compression | Not negotiated; the server does not gzip responses. |
| CORS | Not enabled; the server targets server-side and desktop clients, not browsers on other origins. |

The server is a thin HTTP binding over the desktop command layer: every endpoint calls exactly one engine `*_logic` function (the same functions the Tauri desktop shell calls). DTOs are serialized verbatim from the engine — field names, optionality, and skip-if-empty behavior are identical to the desktop IPC contract (`ipc-ui.md` §1.0). Where the HTTP surface *adds* semantics beyond the desktop command (import jobs, SSE events, `file_ids` defaults), the addition is explicit and documented in §2/§5.

## 2. Endpoints

| # | Method | Path | Request | Success | Typical errors |
|---|--------|------|---------|---------|----------------|
| 2.1 | GET | `/health` | — | 200 `Health` | — (auth-exempt) |
| 2.2 | POST | `/imports` | `CreateImport` | 202 `JobStatus` | 400 |
| 2.3 | POST | `/imports/upload` | multipart | 202 `JobStatus` | 400 |
| 2.4 | GET | `/imports/{job_id}` | — | 200 `JobStatus` | 404 |
| 2.5 | DELETE | `/imports/{job_id}` | — | 200 `JobStatus` | 404 |
| 2.6 | DELETE | `/files/{file_id}` | — | 204 (empty) | 400 |
| 2.7 | GET | `/metrics?file_ids=` | — | 200 `MetricNode[]` | — |
| 2.8 | POST | `/query/series` | `QuerySeries` | 200 `SeriesSlice[]` | 400, 422 |
| 2.9 | POST | `/query/key-values` | `QueryKeyValues` | 200 `KeyValueResult[]` | — (never rejects) |
| 2.10 | GET | `/plugins` | — | 200 `PluginInfo[]` | — |
| 2.11 | GET | `/plugins/{id}/log?limit=` | — | 200 `PluginLog[]` | 400, 404 |
| 2.12 | POST | `/plugins/{id}/reload` | — | 200 `PluginInfo` | 404, 409, 502 |
| 2.13 | POST | `/plugins/install` | multipart | 200 `PluginInfo` | 400, 409, 422 |
| 2.14 | DELETE | `/plugins/{id}` | — | 204 (empty) | 404, 409 |
| 2.15 | PUT | `/plugins/{id}/enabled` | `{"enabled": bool}` | 204 (empty) | 404 |
| 2.16 | GET | `/plugins/{id}/update` | — | 200 `UpdateInfo` | 404, 422, 502 |
| 2.17 | POST | `/plugins/{id}/update` | — | 200 `PluginInfo` | 404, 422, 502 |
| 2.18 | POST | `/sessions/save` | `SaveSession` | 200 `SessionMeta` | 400, 422 |
| 2.19 | POST | `/sessions/load` | `{"path": string}` | 200 `LoadResult` | 400, 404, 422 |
| 2.20 | GET | `/presets` | — | 200 `UserPreset[]` | — |
| 2.21 | POST | `/presets` | `SavePreset` | 201 `UserPreset` | 400, 409 |
| 2.22 | DELETE | `/presets/{id}` | — | 204 (empty) | 400 |
| 2.23 | GET | `/events` | SSE stream | 200 `text/event-stream` | — |
| 2.24 | POST | `/files/{file_id}/queries/{name}` | `{"params"?: object}` (optional body) | 200 `{"data": object}` | 400, 404, 409, 422, 502, 504 |
| 2.25 | GET | `/files/{file_id}/vendor-queries` | — | 200 `{"queries": []}` | 404 |
| 2.26 | GET | `/files` | — | 200 `FileEntry[]` | — |
| 2.27 | POST | `/plugins/rescan` | — | 200 `PluginInfo[]` | — |

Errors are always the uniform envelope of §4; 204 responses carry no body. Status codes are transport-level additions on top of the engine `IpcError.code` (§4) — clients SHOULD key off `code`, not the status.

The service form adds two **gateway-plane** endpoints on the same base path — `POST /api/v1/session` and `DELETE /api/v1/session` (§8.3) — and restricts which instance endpoints are reachable on the public plane (§9.1). Rows 2.26/2.27 above are instance endpoints: 2.26 is an ordinary data-plane endpoint (session-scoped, §9.3); 2.27 belongs to the ops channel and is never exposed anonymously (§10.2).

### 2.1 GET /health

Liveness + version probe. Response:

```json
{ "protocol_version": 1, "version": "0.1.0", "status": "ok" }
```

| Field | Type | Description |
|-------|------|-------------|
| `protocol_version` | integer | Currently fixed to `1` (this document). |
| `version` | string | Server crate version (`CARGO_PKG_VERSION`). |
| `status` | string | Constant `"ok"`; the server answers only when the listener is up. |

This endpoint is exempt from bearer auth (§7.2) so load balancers can probe it.

### 2.2 POST /imports — start an import job

Request (`CreateImport`):

```json
{ "paths": ["C:\\logs\\a.csv", "/data/b.csv"], "overrides": { "C:\\logs\\a.csv": { "plugin_id": "mock" } } }
```

| Field | Type | Description |
|-------|------|-------------|
| `paths` | string[] | REQUIRED. File paths, processed in order, one engine import per path. |
| `overrides` | map&lt;path, `{plugin_id}`&gt; | OPTIONAL. Per-path manual plugin choice (desktop `import_files` `overrides`), bypassing auto-match for that path. |

Response: 202 with a `JobStatus` snapshot (`state: "queued"`). The import runs asynchronously — see §2.4 for the job model.

Per-path semantics are the desktop `import_files` contract: a failed path yields an item with `status: "error"` and an `error` object while other paths continue; the whole request is rejected (`400 invalid_arg`) only when every path is empty. An empty `paths` array is a client error (400).

**Service form:** this path-shaped endpoint is removed from the public plane — the sole ingest channel there is `POST /imports/upload` (§2.3, §9.1). When the instance is started with `--import-roots` (§9.2), every path MUST be confined to the configured roots or the request fails with 403 `path_forbidden`; without the flag (desktop form) local-path capability is unchanged.

### 2.3 POST /imports/upload — upload then import

`multipart/form-data`:

| Field | Type | Description |
|-------|------|-------------|
| `file` | binary | REQUIRED. File content. The part's filename (or the optional `filename` field) becomes the stored basename. |
| `filename` | text | OPTIONAL. Overrides the client-visible filename. |
| `overrides` | text (JSON) | OPTIONAL. Same shape as the `overrides` object of §2.2, keyed by the **client-visible filename** — the stored basename, i.e. the value `ImportResult.name` reports. (Keying by the server-side storage path is not part of the contract: that path is server-generated and client-unpredictable, which is exactly the pre-freeze dead-end where a `needs_user_choice` re-submission could never match.) |

The server writes the bytes to `<OS temp dir>/ab-server-uploads/<pid>-<seq>-<nanos>/<basename>` (uniqueness comes from the directory; the basename preserves the client filename so `ImportResult.name` matches desktop semantics) and then runs the same job flow as §2.2 with that single path. Unknown multipart fields are ignored (forward compatibility). Missing `file` → 400.

The upload copy is retained until its import job reaches a terminal state (completed/failed/cancelled), then deleted — not earlier (the parse may still read it) and not later; the process also sweeps historical residue under its upload root at startup (§7.3, §8.4). **Service form:** this is the sole ingest channel (§9.1); quotas of §9.4 apply.

### 2.4 GET /imports/{job_id} — job status

Import jobs are in-memory; job ids are server-generated (`job-1`, `job-2`, …, monotonic). Lifecycle:

```
queued ──▶ running ──▶ completed
              │  \──▶ failed
              └─────▶ cancelled        (DELETE /imports/{job_id}, §2.5)
```

- `queued`: accepted, waiting for a concurrency slot (`--max-concurrent-imports`, default 2).
- `running`: engine import in progress. Progress percentages flow via SSE (§5), **not** in the job status.
- `completed` / `failed` / `cancelled`: terminal; the `files` array holds the final `ImportResult` list.

A job completes even when individual files need user choice: such items carry `status: "matched"` and `needs_user_choice: true` (desktop contract). The client re-submits via §2.2 with `overrides` for those paths.

`files` may be absent (JSON key omitted) while queued/running. Unknown `job_id` → 404 `file_not_found`. Jobs live for the process lifetime (no persistence, no TTL eviction in v1).

### 2.5 DELETE /imports/{job_id} — cancel

Cooperative cancel, mirroring desktop `cancel_parse`: a queued job is flipped to `cancelled` in place; a running job is flagged and the engine's cancel path is invoked for in-flight parses, so files that already reached Ready stay loaded. Response is the `JobStatus` snapshot after the cancel request (200). Unknown `job_id` → 404. Cancelling an already-terminal job returns its terminal snapshot unchanged.

### 2.6 DELETE /files/{file_id} — unload

Desktop `unload_file` (idempotent; unknown `file_id` still yields 204). Unloaded files disappear from `/metrics`, `/query/*`, and `loaded_file_ids`.

### 2.7 GET /metrics

Query parameter `file_ids`: comma-separated list (`?file_ids=a,b`); absent/empty = **all frozen files**. Returns the desktop `get_metrics` metric tree: file nodes (`level: "file"`) → plugin nodes → metric leaves. Leaf `id` is the composite `file_id:plugin_id:metric_id` used by §2.8.

### 2.8 POST /query/series

Request (`QuerySeries`):

| Field | Type | Description |
|-------|------|-------------|
| `file_ids` | string[] | OPTIONAL. **Server semantics: absent/empty = all frozen files.** (Desktop treats an empty list as an empty allow-list and returns nothing; the server widens this for REST convenience — a documented deviation.) When present, it is the authoritative allow-list: composite metric ids of files not in the list are silently skipped. |
| `metrics` | string[] | REQUIRED. Composite ids `file_id:plugin_id:metric_id`. Malformed ids are ignored (desktop §1.5). |
| `t0_ms`, `t1_ms` | i64 | REQUIRED. Query window, UTC ms. `t0_ms > t1_ms` → 400 `invalid_arg` (desktop parity). |
| `max_points_per_series` | integer | OPTIONAL. Default **4000**; values above **50000** are rejected with 400 `invalid_arg` (server cap; desktop has no HTTP transport to protect). |

Response: `SeriesSlice[]` (§3) — one slice per matched series; `downsampled` reports budget-driven downsampling (desktop parity).

### 2.9 POST /query/key-values

Request: `{ "file_ids": ["..."], "timestamp_ms": 1785603599870 }` — `file_ids` optional with the same server default as §2.8. `timestamp_ms` REQUIRED.

Partial-failure protocol is preserved exactly (desktop `key_values_at`, ipc-ui.md §1.6): the request **never** rejects as a whole; each item is either `{file_id, entries}` or `{file_id, error}`. Per-file timeout is 10 s (desktop parity).

### 2.10–2.17 Plugins

- **GET /plugins** (2.10): discovered plugins merged with live state (desktop `list_plugins`). `state` is the lowercase process state (`discovered`, `spawning`, `loading`, `ready`, …); `disabled` reflects the module-state file (§7.5).
- **GET /plugins/{id}/log?limit=N** (2.11): tail of the plugin's stderr ring buffer (desktop `get_plugin_log`; default 200, clamped to 1..=10000). Unknown plugin id → 404.
- **POST /plugins/{id}/reload** (2.12): rebuild the plugin session; `plugins-reloaded` SSE frames are broadcast (§5). The plugin's already-loaded files are re-opened through the import pipeline.
- **POST /plugins/install** (2.13): `multipart/form-data` with `file` (plugin ZIP) and optional `overwrite` (`"true"`/`"1"`/`"yes"`/`"on"`, case-insensitive). Installs into the portable plugins dir (desktop `install_plugin_zip`); the temp copy is deleted after the call. Conflict without overwrite → 409.
- **DELETE /plugins/{id}** (2.14): uninstall (close session → kill process → remove dir → registry reload). Built-in plugins are protected → 409.
- **PUT /plugins/{id}/enabled** (2.15): body `{"enabled": bool}`; persists the module-state file and enables/disables the plugin (spec §4.4). Disabling triggers a reload broadcast.
- **GET /plugins/{id}/update** (2.16): GitHub Releases check (desktop `check_plugin_update`). No newer release → 422 `update_not_available`.
- **POST /plugins/{id}/update** (2.17): download + overwrite-install + reopen loaded files (desktop `update_plugin`). Returns the fresh `PluginInfo`.

The update source is fixed to GitHub Releases in v1 (`GitHubFetcher`); the server constructs it at startup and fails fast if unavailable.

### 2.18–2.19 Sessions

- **POST /sessions/save** (2.18): body `{ "path"?: string, "snapshot"?: SessionSnapshot }`. Path resolution: omitted → auto-named `session-<pid>-<seq>-<nanos>.absession` inside `--sessions-dir`; relative → joined onto `--sessions-dir`; absolute → MUST normalize (lexically) to inside `--sessions-dir`, otherwise 400 `invalid_arg` (path-confinement, §7.4). Returns `SessionMeta`.
- **POST /sessions/load** (2.19): body `{ "path": string }` (same constraint). Re-opens the session and re-imports every verified file through the pipeline (desktop `load_session`). **Desktop parity note:** `load_session` re-runs the import pipeline for all session files — a file that is *already loaded* in the running session reports `reopen_failed`; clients SHOULD unload files before loading a session that contains them.
- **Service form:** this path-shaped endpoint is removed from the public plane together with §2.2's path form (§9.1); when the instance runs with `--import-roots`, `path` MUST fall inside the configured roots, else 403 `path_forbidden` (§9.2).

### 2.20–2.22 User presets

- **GET /presets** (2.20): all user presets, sorted by id.
- **POST /presets** (2.21): body `{ "name": {"zh": "...", "en": "..."}, "entries": { "<plugin_id>": ["<metric>", ...] } }`. Both `name.zh` and `name.en` must be non-empty after trimming. The id is derived from `name.zh` (slug, `^[a-z0-9][a-z0-9-_]{0,63}$`); an existing preset with the same id → 409 `preset_conflict`. Returns 201 with the stored `UserPreset`.
- **DELETE /presets/{id}** (2.22): idempotent (unknown but well-formed id → 204); malformed id → 400.

### 2.23 GET /events — SSE

See §5.

### 2.24 POST /files/{file_id}/queries/{name} — vendor named query

Vendor-defined **named query** (CCP-custom-query; protocol-v1.md §2.11). Neutrality rule: the caller only names the file — the server resolves `file_id → plugin_id`; `name` and the payload are **opaque** to the server, which never inspects, validates, or interpolates them.

Request body is optional (empty/absent body = no params):

```json
{ "params": { "window": 60 } }
```

| Outcome | Status | `error.code` |
|---------|--------|--------------|
| Success | 200 | — (body `{ "data": <object> }`; vendor-defined payload, MAY be empty but MUST be an object) |
| Plugin does not declare `custom_query` (incl. legacy plugins replying `-32601`) | 422 | `unsupported` |
| Unknown `query` name (`-32602`) | 422 | `invalid_params` |
| Target file mid-parse (`-32001`) | 409 | `plugin_busy` |
| 10s budget exhausted | 504 | `timeout` |
| Plugin subprocess died | 502 | `plugin_crashed` |
| Unknown `file_id` | 404 | `file_not_found` |
| Malformed JSON body | 400 | `invalid_arg` |

The `unsupported`/`invalid_params` normalization is engine-side (§2.11 of protocol-v1.md): legacy `-32601` maps to the same `unsupported` outcome — never `internal`.

### 2.25 GET /files/{file_id}/vendor-queries

Discovery of named queries. v1 Phase 2 has no discovery method (`list_queries` is a deferred optional method, CCP-custom-query Phase 3) — the endpoint answers `200 { "queries": [] }` for any loaded file and 404 `file_not_found` otherwise; it will surface real listings additively once Phase 3 lands. Clients MUST tolerate the empty listing.

### 2.26 GET /files — loaded-file inventory

Returns the files currently held by **this instance** — in the service form, exactly this session's files (§9.3). One `FileEntry` (§3) per loaded file. The population is the same as the file nodes of `/metrics` (§2.7): files unloaded via §2.6 disappear; an entry exists once an import (§2.2/§2.3/§2.19) has registered the file, including files still `parsing`. This closes the pre-freeze gap where a session-scoped client had no reliable way to enumerate its own files (audit A1 P3).

### 2.27 POST /plugins/rescan — full registry rescan (ops channel)

Triggers `registry.reload()` — the same full plugin-directory rescan that runs at startup and after a successful install (discovery → manifest validation → builtin registry merge). Unlike `POST /plugins/{id}/reload` (§2.12), which rebuilds one plugin's sessions without re-reading directories, this endpoint re-reads the **directories** and is the repair path for a registry that has drifted from disk (broken/missing builtins — audit A5-P1-7). Response: 200 with the post-rescan `PluginInfo[]`; a `plugins-reloaded` SSE frame (§5) is broadcast.

Reachability: in the service form this endpoint belongs to the ops channel and MUST NOT be exposed to the anonymous public plane (§10.2); in the desktop form it is a local maintenance call.

## 3. DTO Reference

All DTOs below are the engine command DTOs serialized verbatim (`core/ab-engine/src/commands/*`); **shapes are identical to the desktop contract `ipc-ui.md` §1.0**, including optional-field omission (a `None`/empty value means the JSON key is absent, not `null` — except where a field is documented as explicitly nullable, e.g. `PluginInfo.last_error`).

### IpcError (error envelope payload, §4)

| Field | Type | Present |
|-------|------|---------|
| `code` | string | always |
| `message` | string | always |
| `data` | object | optional (omitted when none) |

### JobStatus (server-owned, §2.4)

| Field | Type | Present |
|-------|------|---------|
| `job_id` | string | always |
| `state` | string | always — `queued` / `running` / `completed` / `failed` / `cancelled` |
| `error` | IpcError | optional (failed jobs) |
| `files` | ImportResult[] | optional (omitted while empty; terminal jobs carry the full per-path list) |

### ImportResult (desktop §1.0 `ImportResult`)

| Field | Type | Present |
|-------|------|---------|
| `file_id` | string | always (empty string when not ready) |
| `path` | string | always |
| `name` | string | always (file basename) |
| `size_bytes` | integer | always |
| `status` | string | always — `matched` / `parsing` / `ready` / `error` |
| `matched_plugin` | PluginMatch | optional |
| `candidate_plugins` | PluginMatch[] | always (possibly empty) |
| `needs_user_choice` | boolean | optional (`true` when manual choice is required) |
| `error` | IpcError | optional |
| `time_range` | `{start_ms, end_ms}` | optional (ready files only) |

`PluginMatch`: `{ plugin_id: string, confidence: number, reason?: string }`.

### FileEntry (server §2.26)

| Field | Type | Present |
|-------|------|---------|
| `file_id` | string | always |
| `name` | string | always (basename; the same value `ImportResult.name` reports) |
| `size_bytes` | u64 | always |
| `status` | string | always — last import status: `matched` / `parsing` / `ready` / `error` |
| `source` | string | always — `upload` (§2.3) / `path` (§2.2) / `session` (§2.19 re-open) |

### MetricNode (desktop §1.0 `MetricNode`)

| Field | Type | Present |
|-------|------|---------|
| `level` | string | always — `file` / `plugin` / `metric` |
| `id` | string | always (file id / `<file>:<plugin>` / `<file>:<plugin>:<metric>`) |
| `file_id` | string | always |
| `plugin_id`, `metric_id` | string | optional (level-dependent) |
| `name` | string | always |
| `unit`, `description`, `aggregation` | string | optional |
| `children` | MetricNode[] | optional |

### SeriesSlice (desktop §1.0 `SeriesSlice`)

| Field | Type | Present |
|-------|------|---------|
| `file_id`, `plugin_id`, `metric_id` | string | always |
| `point_count` | integer | always |
| `downsampled` | boolean | always |
| `points` | `{t_ms: i64, v: number}[]` | always |

### KeyValueResult (desktop §1.0 `KeyValueResult`)

| Field | Type | Present |
|-------|------|---------|
| `file_id` | string | always |
| `entries` | `{key: string, value: string}[]` | optional (success) |
| `error` | IpcError | optional (per-file failure; `entries` absent) |

### CustomQueryResult (server §2.24, CCP-custom-query)

| Field | Type | Present |
|-------|------|---------|
| `data` | object | always (vendor-defined payload; opaque to the server; MAY be empty) |

### PluginInfo (desktop §1.0 `PluginInfo`)

| Field | Type | Present |
|-------|------|---------|
| `id`, `display_name`, `version` | string | always |
| `state` | string | always (lowercase process state) |
| `loaded_file_ids` | string[] | always |
| `capabilities` | `{annotate, subscribe, binary_sidecar}` (booleans) + optional `custom_query` | always — `annotate`/`custom_query` report the plugin's real initialize answer (CCP-custom-query; absent key = `custom_query: false`); `subscribe`/`binary_sidecar` are fixed-false v1 placeholders |
| `last_error` | string \| null | always (may be `null`) |
| `source` | string | always — `portable` / `user` |
| `builtin`, `disabled` | boolean | always |
| `update_url`, `author`, `repository` | string | optional |
| `tools` | string[] | optional |
| `changelog`, `presets` | array | optional (manifest passthrough) |

### UpdateInfo (desktop `check_plugin_update` result)

| Field | Type | Present |
|-------|------|---------|
| `plugin_id`, `current_version` | string | always |
| `latest_version` | string | optional |
| `is_newer` | boolean | always |
| `asset_name` | string | optional |

### SessionMeta / LoadResult (desktop §1.0)

`SessionMeta`: `{ path: string, saved_at_ms: i64, file_count: integer, selected_metric_count: integer }`.

`LoadResult`:

| Field | Type | Present |
|-------|------|---------|
| `session` | SessionMeta | always |
| `loaded_file_ids` | string[] | always |
| `missing` | `{path, reason}[]` | always (reason `not_found` / `hash_mismatch`) |
| `reopen_failed` | `{path, reason}[]` | optional (reason `reopen_failed`) |
| `time_ranges` | `{file_id, start_ms, end_ms}[]` | optional |
| `snapshot` | SessionSnapshot | optional |
| `files` | ImportResult[] | optional (full results of re-opened files) |

### UserPreset (desktop `list_user_presets` item)

| Field | Type | Present |
|-------|------|---------|
| `id` | string | always (`^[a-z0-9][a-z0-9-_]{0,63}$`; derived from `name.zh`) |
| `name` | `{zh, en}` | always |
| `description` | `{zh, en}` | optional |
| `entries` | map&lt;plugin_id, string[]&gt; | always |

## 4. Error Model

Every error response (any 4xx/5xx) has the body:

```json
{ "error": { "code": "invalid_arg", "message": "human-readable detail", "data": null } }
```

`code`/`message` are the desktop `IpcError` fields verbatim; `data` is an optional JSON object and is omitted when absent. The HTTP status is derived from `code` by the server (the only implementation is `core/ab-server/src/error.rs::status_for`):

| `code` | HTTP status | Notes |
|--------|-------------|-------|
| `invalid_arg` | 400 | Malformed request body/params. |
| `session_required` | 400 | **Gateway plane** (§8.3): non-GET request without a valid session cookie. |
| `path_forbidden` | 403 | Instance (§9.2): an ingest path parameter falls outside `--import-roots`. |
| `permission_denied` | 403 | **Gateway plane** (§10.1): authenticated but not `sysadmin` on a plugin-management route. |
| `session_not_found` | 404 | **Gateway plane** (§8.5): expired/unknown sid. |
| `file_not_found` | 404 | Unknown file/job/session path. |
| `module_not_found` | 404 | Unknown plugin id. |
| `plugin_busy` | 409 | Concurrent per-plugin operation; retry later. |
| `cancelled` | 409 | Chosen over nginx-private 499; the op lost a race or was cancelled. |
| `module_conflict` | 409 | e.g. install without `overwrite`. |
| `module_protected` | 409 | Built-in plugin uninstall/overwrite. |
| `module_in_use` | 409 | Plugin has loaded files. |
| `preset_conflict` | 409 | Preset id already exists. |
| `parse_failed` | 422 | Pipeline parse error. |
| `file_load_failed` | 422 | load_file failure. |
| `module_install` | 422 | ZIP layout/validation failure. |
| `update_not_available` | 422 | No newer release. |
| `unsupported` | 422 | Structurally valid request for an unsupported capability (incl. `custom_query` on a plugin without the capability, and legacy `-32601`). |
| `invalid_params` | 422 | Plugin-side semantic invalidity (`custom_query` unknown query name, `-32602`). |
| `plugin_crashed` | 502 | Plugin subprocess died (bad gateway semantics). |
| `network` | 502 | Update fetch network failure. |
| `timeout` | 504 | Engine timeout budget exhausted. |
| `memory_budget_exceeded` | 413 | Engine memory budget hard cap (`--memory-budget-mb`) exceeded by resident store bytes after parse; the file was unloaded and only that file's import outcome fails. |
| `upload_too_large` | 413 | Single upload exceeds the 64 MB request-body limit (§9.4). The transport-level body-limit rejection MUST use this envelope (pre-freeze implementations flattened it to a bare 400 — audit A1 P3). |
| `file_limit_reached` | 429 | Concurrent loaded files for this session at the cap (§9.4). |
| `upload_quota_exceeded` | 429 | Cumulative uploaded bytes for this session at the cap (§9.4). |
| `session_io`, `state_io`, `internal`, `host_backpressure` | 500 | Server/engine-side failures. |
| Numeric codes (`-32700`…`-32602`-style) | 400 | JSON-RPC-style transport codes passed through. |
| `tenant_capacity` | 503 | **Gateway plane** (§8.5): global instance capacity reached with no idle victim; response carries `Retry-After`. |
| Unknown code | 500 | Conservative fallback; new engine codes arrive before server releases in practice. |

The `code` field is the stable machine-readable identity; the status table is a convenience mapping and MAY be extended additively within `/api/v1`. **Gateway-plane** codes (marked above) use the same envelope shape; their fact source is the gateway implementation (`AnalysisBuddy_WebUI/server/auth-gateway.js`), not the engine `IpcError`.

## 5. Events (SSE)

`GET /api/v1/events` upgrades the response to `text/event-stream` (HTTP/1.1 chunked). Frames:

```
event: progress
data: {"file_id":"…","percent":42.0,"records_so_far":1200,"bytes_read":null}

event: plugin-health
data: {"plugin_id":"mock","state":"ready","prev_state":"loading","detail":null}
```

- Frame **name** is the engine channel name with the `ab://` prefix stripped (short names below). The payload is the inner event object serialized as JSON (no wrapper envelope).
- Comment keep-alive lines (`:`) are emitted periodically (~15 s) by the transport and MUST be ignored by clients.

### Channels

| Frame name | Engine channel (`ab_engine::events`) | Payload |
|------------|--------------------------------------|---------|
| `progress` | `EV_PROGRESS` (`ab://progress`) | `{file_id, percent?: number, records_so_far: integer, bytes_read?: integer}` |
| `plugin-log` | `EV_PLUGIN_LOG` (`ab://plugin-log`) | `{plugin_id, level, line, ts_ms}` |
| `plugin-health` | `EV_PLUGIN_HEALTH` (`ab://plugin-health`) | `{plugin_id, state, prev_state, detail?: string}` |
| `plugins-reloaded` | `EV_PLUGINS_RELOADED` (`ab://plugins-reloaded`) | `{plugins: string[], invalid: string[], shadowed: string[]}` |

`level` is `info`/`warn`/`error` (parsed from the stderr line prefix; unprefixed lines are `info`).

### Filters and throttling

- Query `?file_id=` filters `progress` frames to that file; `?plugin_id=` filters `plugin-health`/`plugin-log` frames to that plugin. Filters combine (AND across kinds).
- Progress throttling is **per subscriber**: each connection applies the desktop 100 ms-per-file throttle locally; terminal progress (`percent >= 100`) always passes. Two subscribers therefore see identical *ordering* but not identical *frame counts*.

### Architecture and backpressure

The server runs one forwarding task that consumes host events and pipeline events, converts them via the desktop `events::convert` / `convert_pipeline` mapping (no throttling at this stage), and publishes to a central hub (ring buffer 512). Each SSE connection owns an independent forwarding queue (256 entries) filled from the hub.

If a subscriber falls behind and its hub slice overflows, the server does **not** silently drop frames: the affected stream receives a terminal error frame and is then closed:

```
event: error
data: {"code":"event_stream_lagged","message":"subscriber fell behind; connection closed to avoid silent gaps","skipped":42}
```

`skipped` is the number of hub events the subscriber missed. Clients MUST treat this frame as end-of-stream and re-subscribe if they still need events. All other subscribers are unaffected.

There is no cross-channel ordering guarantee (host-state and pipeline events travel through different sources), and no replay: events published before a subscription are not delivered.

## 6. Versioning

- The base path `/api/v1` is **additive-only**: new endpoints, new optional request fields, and new SSE channels MAY be added without a version bump. Existing field names, semantics, and error codes MUST NOT change within `/api/v1`.
- Breaking changes (field removal/renaming, semantic changes, removal of endpoints) REQUIRE a new base path (`/api/v2`) served alongside `/api/v1` for a deprecation window.
- `GET /health.protocol_version` reports `1` for this contract. Clients SHOULD tolerate unknown SSE frame names and unknown response fields (forward compatibility).

## 7. Security & Deployment

### 7.1 Network exposure

The server binds `127.0.0.1:8600` by default and is intended for loopback use (local tooling, embedded UIs). For remote deployment, front it with a reverse proxy that terminates TLS; use `--addr 0.0.0.0:8600` (or a specific interface) deliberately. In the **service form** (§8) instances stay loopback-only behind the gateway; Appendix A records the current gateway→instance interface.

### 7.2 Authentication

`--token <token>` enables bearer auth: every request MUST carry `Authorization: Bearer <token>` except `GET /api/v1/health`. Failures get 401 with the uniform envelope (`code: "unauthorized"`). Token comparison is constant-time. Without `--token`, no auth is enforced — suitable only for loopback deployments. Deployments SHOULD additionally restrict at the network layer (bind address, firewall, proxy ACLs); bearer tokens are not a substitute for transport security.

### 7.3 Threat model

- **Plugins are arbitrary code.** They run as subprocesses with the server's OS privileges. Installing a plugin (§2.13) is therefore equivalent to granting code execution: restrict `/plugins/install` and `/plugins/{id}/update` to trusted operators (auth + network policy). Plugin ZIPs are validated for manifest/layout compliance, not for safety.
- **Uploads** land in the OS temp dir (`<temp>/ab-server-uploads/…`, i.e. under the process `TMPDIR`) and are imported like local files. The upload copy is deleted when its import job reaches a terminal state — not earlier (the parse may still be reading it) and not later; the process also sweeps historical residue under its upload root at startup (§2.3; part of the session teardown contract, §8.4). The request body limit is 64 MB. (A pre-freeze revision of this section claimed "each upload copy is removed after the import call"; no such removal existed — audit P0-4. That claim is retracted.)
- **Path confinement:** session save/load paths are confined to `--sessions-dir` (§2.18); uploads are confined to the server-managed upload dir; preset ids are charset-restricted. **However, `POST /imports` (§2.2) and `POST /sessions/load` (§2.19) do accept caller-supplied filesystem paths** — that is the deliberate local-tooling capability of the same binary. The service form bounds it with two layers: the gateway removes both path-shaped forms from the public plane (§9.1), and an instance started with `--import-roots` rejects any path outside the configured roots with 403 `path_forbidden` (§9.2). (A pre-freeze revision of this section claimed "No endpoint accepts arbitrary server-side file reads"; that claim was false — audit P0-2 — and is retracted.)
- **Denial of surface:** the concurrency gate (`--max-concurrent-imports`, default 2) bounds parallel parse load; SSE subscriber queues are bounded and lagging subscribers are disconnected (§5); the optional engine memory budget (`--memory-budget-mb`) caps the resident store bytes — an import that would exceed it fails per-file with `memory_budget_exceeded` (413) and is unloaded, instead of growing memory without bound. The check runs after parse freeze, so the peak may transiently exceed the budget; the budget bounds post-load residency (approximate accounting, order-of-magnitude accurate).

### 7.4 Data directories

`--user-data-dir` sets three roots at once (`{plugins,presets,sessions}` subdirectories); individual flags override. Defaults:

| Platform | Plugins portable/install | Plugins user | Presets | Sessions |
|----------|--------------------------|--------------|---------|----------|
| Linux (and other non-Windows) | `$XDG_DATA_HOME/AnalysisBuddy/plugins` (fallback `$HOME/.local/share/AnalysisBuddy/plugins`) | same path | same root `/presets` | same root `/sessions` |
| Windows (dev parity with the desktop shell) | `<exe dir>\plugins` | `%APPDATA%\AnalysisBuddy\plugins` | `%APPDATA%\AnalysisBuddy\presets` | `%APPDATA%\AnalysisBuddy\sessions` |

Sessions save/load MUST stay inside `--sessions-dir` (§2.18).

### 7.5 Linux deployment notes

- `ab-server` is a plain tokio binary; no GUI stack, no WebView2, no Tauri runtime.
- Plugins MUST provide Linux entry points: the manifest `entry.command` is resolved per protocol-v1 §7.3 (absolute path or PATH lookup). A plugin shipping only Windows binaries will fail discovery/startup on Linux.
- The module-state file (disabled-plugin set) lives in the portable plugins dir; seed it before startup to boot with plugins disabled (desktop spec §4.4).
- Process supervision: `SIGINT` (Ctrl+C) triggers graceful shutdown — the HTTP listener stops accepting, then all plugin subprocesses are shut down and reaped. Run under systemd/supervisord with `Restart=on-failure` for production.
- Observability: stdout/stderr carry the server's own log lines (`listening on …`, assemble errors); plugin stderr is captured per-plugin and exposed via `plugin-log` SSE frames and `/plugins/{id}/log` (not written to server stdout).

## 8. Session Model (Service Form)

Normative text for the service form ruled by master plan vol-1 §1.3–§1.4 (`docs/audit-and-dev-master-plan-2026-09-27.md`). The **session** is the unit of isolation: one browser session or one API-client session ≡ one exclusive ab-server instance (own port, own token, own store/TMPDIR/data dirs). Security in this form is *session isolation, not authentication*: analysis endpoints stay anonymous; only plugin management is authenticated (§10).

### 8.1 Topology

```
browser / API client ──HTTP──▶ nginx(:8601) ──▶ auth-gateway(:8602, loopback, only boundary)
                                                  │ one exclusive ab-server instance per session
                                                  ▼
                                    ab-server (127.0.0.1:8610+, random --token)
```

The gateway is the only security boundary. Instances are loopback-only with per-instance random bearer tokens injected server-side (never sent to the browser; client `Authorization` headers are dropped at the gateway). Appendix A records the current gateway→instance interface and its deltas against this section.

### 8.2 Session identity: the `ab_sid` cookie

| Property | Value |
|---|---|
| Value | ≥128-bit CSPRNG, hex-encoded (≥32 hex chars); minted **server-side** by the gateway (never accepted from the client) |
| Attributes | `HttpOnly`; `Path=/`; `SameSite=Lax`; `Secure` once TLS is enabled (freeze-day deployments are plain HTTP; adding `Secure` is M1 scope); `Max-Age` ≤ 43200 (12 h absolute) |
| Legacy `ab_tenant` | Client-supplied values are **ignored without exception** — the cookie MUST NOT influence routing or isolation. (Pre-freeze, the client-supplied value *was* the credential — audit A4#3; this contract supersedes it.) |
| Capability semantics | `ab_sid` is a bearer capability: possession = the session's entire data plane. It binds to no TCP connection, no identity, and no other session, and is never merged across sessions. **sid loss = data loss** after idle-TTL reclaim, with no cross-session recovery — clients design their retry logic accordingly. |

### 8.3 Creating a session

Gateway-plane endpoints (same `/api/v1` base path, same §4 envelope):

| Method | Path | Success | Errors |
|---|---|---|---|
| POST | `/api/v1/session` | 201 `{"sid": "<hex>"}` + `Set-Cookie: ab_sid=…` | 503 `tenant_capacity` (+`Retry-After`) at the global cap |
| DELETE | `/api/v1/session` | 204 (empty) + `Set-Cookie: ab_sid=; Max-Age=0` | — (idempotent: unknown/expired sid also returns 204) |

- **Explicit creation** is the standard entry point for API clients. `POST /api/v1/session` always mints a **new** session (an existing sid is not echoed back), so a client may hold several sids concurrently (§8.5). The body carries the sid for cookie-jar-less clients; the cookie serves cookie-jar clients — both are first-class.
- **Implicit minting exists only as the read-only GET bootstrap**: a GET request without a valid sid is answered with a newly minted sid via `Set-Cookie` and proceeds normally (a browser's first load completes transparently). The instance itself is still spawned lazily on the first data-plane request.
- **Any non-GET request without a valid sid → 400 `session_required`** (§4). This deliberately closes the classic footgun where a cookie-less script silently creates a fresh session per request and its uploaded data "disappears" on the next call.
- A request carrying a sid that was terminated or expired → 404 `session_not_found` (§8.5).

### 8.4 Terminating a session

All six termination triggers funnel through **one idempotent gateway function, `teardownSession(sid)`**:

1. **explicit end** — `DELETE /api/v1/session` (and the UI "end session" button);
2. **idle TTL expiry** — no in-flight request and last activity older than the TTL (default 10 min; busy sessions are never reaped);
3. **absolute lifetime expiry** — 12 h after minting regardless of activity (long analyses must re-create the session and re-upload; clients are told so here);
4. **capacity eviction** — only idle sessions may be evicted; at the cap with no idle victim the next session creation fails 503 `tenant_capacity` + `Retry-After`;
5. **gateway restart/shutdown** — all sessions terminate; startup performs the full sweep below;
6. **crashed/orphaned instance** — the reaper reclaims the record and directories.

`teardownSession` cleanup matrix (the normative cleanup contract):

| Artifact | Action | Verification |
|---|---|---|
| ab-server subprocess | SIGTERM → 3 s grace → SIGKILL; the port is returned **only after process exit** (pre-freeze grace: 5 s; port-return-on-exit is already enforced) | no process whose command line mentions the sid |
| session TMPDIR (`<AB_TENANT_TMP_ROOT>/<sid>`, default `/dev/shm/ab-tenants/<sid>`) | `rm -rf` | directory absent |
| session data dir (`<AB_TENANT_ROOT>/<sid>`: sessions/presets) | `rm -rf` (presets are client-side per master plan §1.7 — no cross-session server-side user data) | directory absent |
| in-memory store / jobs / diagnostics | die with the process | — (process death clears them) |
| upload copies (`<TMPDIR>/ab-server-uploads/…`) | deleted at import-job terminal state, independent of session end (§2.3, §7.3) | soak asserts 0 residue |
| cookie | `Set-Cookie: ab_sid=; Max-Age=0` (on non-DELETE triggers this is emitted with the client's next gateway response) | — |

**Invariant I-1 (machine-checked):** within 60 s of any termination event, the sid's artifact set — the process **and** both directories — is empty. Verified by the residue checker under the chaos benchmark (master plan vol-2 §2.7). Pre-freeze retention behavior (tenant data dirs kept 7 days, orphan tmp dirs swept after 1 h) is superseded by this contract.

Gateway startup = full sweep: scan `<AB_TENANT_TMP_ROOT>/*`, `<AB_TENANT_ROOT>/*`, and leftover ab-server processes; clear all of them (idempotent, logged).

### 8.5 API-session lease semantics

- The sid is an explicit lease in the file-handle model: **explicit open** (§8.3), **explicit close** (`DELETE /api/v1/session`), **timeout reclaim** (idle TTL / 12 h absolute). Client crash, network loss, or a forgotten close is covered by the TTL safety net.
- Requests with an expired/unknown sid → 404 `session_not_found`. The correct client recovery is **re-create the session and re-upload** — the contract states plainly that there is no cross-session data recovery, so clients do not misread it as "data loss".
- One client may hold multiple sids simultaneously = fully isolated sandboxes (the recommended shape for parallel analyses). Concurrent requests on one sid share that instance's files (equivalent to several browser tabs).
- "in-flight" counts as active: **idle** = no in-flight request AND the last request is older than the TTL.
- Abuse gate: the anonymous model lets any reachable peer open sessions up to global capacity; 503 `tenant_capacity` + `Retry-After` is the baseline gate (a per-source-IP concurrent-session cap MAY be added later, default off — master plan decision D-2).

## 9. Ingestion and the Session Namespace (Service Form)

### 9.1 Sole ingest channel; path forms removed from the public plane

- The **only** ingest channel on the public plane is `POST /imports/upload` (§2.3, multipart).
- `POST /imports` with body `paths` (§2.2) and `POST /sessions/load` with body `path` (§2.19) are **removed from the public plane**: the gateway answers 404 (`not_found`) or 405 before any instance is reached. Clients MUST NOT be able to make either succeed through the gateway.
- This is a **routing-plane restriction, not a protocol removal**: both endpoints remain in ab-server's `/api/v1` surface for the desktop form, so the additive-only rule of §6 is not breached.
- Desktop form: the same binary, run without the gateway and without `--import-roots`, keeps full local-path ingest capability.

### 9.2 `--import-roots` — defense-in-depth path confinement

New instance startup flag (accepted alongside the flags of §7.4):

```
--import-roots <dir>[,<dir>]   Restrict ingest path parameters to the listed
                               roots (comma-separated). Default: unset = no
                               restriction (desktop form).
```

- Scope: every caller-supplied path parameter of §2.2 (`paths[]`) and §2.19 (`path`). (`/sessions/save` stays independently confined to `--sessions-dir`, §2.18; upload imports target the server-managed upload dir, §2.3.)
- Check = **lexical normalization, then component-level containment**: lexically resolve the path (reject forms that escape via `..`), then require the normalized path to have one of the roots as a **path-component** prefix — not a byte prefix (`/root/a` does not match root `/root/ab`). Same style as the existing sessions-dir confinement. Symlinks are not resolved in v1 (lexical check only — documented limitation).
- Violation → 403 `path_forbidden` (§4) before any file is opened.
- Flag absent → local-path capability is fully retained. One binary, two forms.
- Gateway duty: start each instance with its own upload root as the sole import root, so that even a buggy or compromised routing layer cannot turn instance ingest into arbitrary file reads.

### 9.3 Session-scoped file visibility

Files from other sessions are unreachable by construction: separate processes, stores, and directories per instance. `GET /files` (§2.26) is the authoritative "my files" listing for a session; `/metrics` file nodes (§2.7) cover the same population in tree form. Neither ever spans sessions.

### 9.4 Quotas

| Quota | Default | Override | Exceeded → |
|---|---|---|---|
| Single upload size | 64 MB (the §1 request-body limit) | — (transport limit) | 413 `upload_too_large` (envelope, §4) |
| Concurrent loaded files / session | 32 | gateway env `AB_SESSION_MAX_LOADED_FILES` → instance flag `--max-loaded-files` (0 = unlimited) | 429 `file_limit_reached` |
| Cumulative uploaded bytes / session | 512 MB | gateway env `AB_SESSION_UPLOAD_QUOTA_MB` → instance flag `--upload-quota-mb` (0 = unlimited) | 429 `upload_quota_exceeded` |

- The 429 bodies SHOULD carry `data` with the limit and (byte quota) the consumed-so-far value, so clients can free quota via `DELETE /files/{file_id}` (§2.6) or end the session instead of retrying blindly; `Retry-After` is not meaningful for quota rejections and MAY be omitted.
- The per-instance memory budget (`--memory-budget-mb`, gateway env `AB_TENANT_MEMORY_MB`) is unchanged (§7.3: 413 `memory_budget_exceeded`, per-file).
- The count/byte quotas are enforced **at the instance**, so the contract also holds for direct (non-gateway) deployments; the gateway merely configures them per session.

## 10. Plugin Management Channels

Plugin install/update/uninstall is the **only authenticated surface** in the service form (master plan §1.1). Two equivalent channels:

### 10.1 Admin channel (gateway, DQA roles)

Gateway routes under `/api/v1/plugins*` require a valid admin login, and the role mapping MUST yield `sysadmin`: an authenticated non-sysadmin receives 403 `permission_denied` (§4). (Pre-freeze the gate checked only "logged in" — audit P0-1; the role check is normative as of this freeze.) Administrative operations are audit-logged (who / plugin / action / result timeline, JSON lines on disk).

### 10.2 Ops channel (host-side, root)

An operator with root/sudo on the server may manage plugin directories directly and invoke `POST /plugins/rescan` (§2.27) — either directly against an instance (loopback, its token) or through the admin route — to trigger the full registry rescan. This is the designated repair path for broken or missing builtins, and it removes the pre-freeze dependency on side-effect hacks (broadcasting `PUT /enabled` to force a rescan).

### 10.3 Builtin repair install

`module_protected` (§2.13/§2.14) normally blocks installing over a builtin id. **Repair exception:** installing a builtin id is allowed when its deployed directory entry is **absent**, or its manifest is **invalid** (unreadable or failing validation). A repair install runs the normal install pipeline and triggers the registry reload (§2.27). This unblocks the "id in the compile-time builtin list but missing/broken in the deploy dir" deadlock (audit A5-P1-7: `409 module_protected` even with `overwrite=true`, and reload never rescanning directories).

### 10.4 Install-is-usable

An install/update that returns 200 MUST be backed by an initialize smoke test (spawn → handshake → shutdown, 5 s budget); on failure the installation rolls back and the error response carries the plugin's stderr tail. "Install 200 ≠ usable" (audit P0-3-3, the Python-SDK `ModuleNotFoundError` incident) is closed by this normative requirement (implementation: master plan E3).

## Appendix A. Gateway–Instance Interface (current state, 2026-09-27)

Read-only facts recorded from `AnalysisBuddy_WebUI/server/auth-gateway.js` (`startTenant`, config block) and `core/ab-server/src/args.rs`, for implementers of §8–§9. The deltas to reach this contract are listed last.

**Flags the gateway passes to each spawned instance:**

| Flag | Current value | Source (env, default) |
|---|---|---|
| `--addr` | `127.0.0.1:<port>`, first free port scanning from 8610 | `AB_TENANT_PORT_BASE` (8610) × `AB_TENANT_PORT_SPAN` (400); the port returns to the pool only on process `exit` |
| `--token` | 48-hex (192-bit) `crypto.randomBytes(24).toString('hex')`, per instance | — |
| `--max-concurrent-imports` | per-tenant value | `AB_TENANT_MAX_IMPORTS` (2) |
| `--memory-budget-mb` | per-tenant value | `AB_TENANT_MEMORY_MB` (512; 128 on the 2 GB host) |
| `--sessions-dir` | `<tenant-root>/<tid>/sessions` | `AB_TENANT_ROOT` (`/var/lib/analysisbuddy/tenants`) |
| `--presets-dir` | `<tenant-root>/<tid>/presets` | same |
| `--plugins-portable` | shared dir (all instances) | `AB_PLUGINS_PORTABLE` (`~/.local/share/AnalysisBuddy/plugins`) |
| `--plugins-user` | shared dir (all instances) | `AB_PLUGINS_USER` (`/var/lib/analysisbuddy/plugins`) |

**Environment / process shape:** `TMPDIR=<AB_TENANT_TMP_ROOT>/<tid>` (`AB_TENANT_TMP_ROOT` default `/dev/shm/ab-tenants`, a memory FS); `cwd` = the tenant dir; stdout + stderr → `<tenant-dir>/server.log` (truncated past `AB_TENANT_LOG_MAX_BYTES`, 8 MB; the parent closes its fd copies after spawn). Not passed today: `--user-data-dir` (individual `--sessions-dir`/`--presets-dir` are used instead), `--plugins-install`, `--import-roots` (§9.2, to be added). ab-server has no `--port`/`--bind` flag — `--addr` is the single listener flag (args.rs).

**Health gate:** `GET /api/v1/health` with the instance token — poll every 80 ms, 1.5 s per-attempt timeout, overall `AB_TENANT_START_TIMEOUT_MS` (15 s). Start failure → SIGKILL + tmp cleanup + 503 `tenant_start_failed` to the client.

**Lifecycle knobs:** `AB_MAX_TENANTS` (24; 4 on the 2 GB host), `AB_TENANT_IDLE_TTL_MS` (600 000), eviction idle floor 5 s (busy sessions skipped), reaper interval 30 s, orphan-tmp sweep after 1 h, stale tenant-dir sweep after 7 days. Current teardown grace is 5 s (target 3 s, §8.4).

**Cookies:**

| Cookie | Issuer / purpose | Current attributes | Per this contract |
|---|---|---|---|
| `ab_sid` | gateway, session identity | *does not exist yet* (master plan WS-A3) | §8.2: ≥128-bit CSPRNG, `HttpOnly`, `SameSite=Lax`, `Secure` (TLS), `Max-Age` ≤ 43 200 |
| `ab_session` | gateway, DQA login session (admin plane) | `HttpOnly`; `Path=/`; `SameSite=Lax`; `Max-Age` 43 200 (`AB_SESSION_TTL_MS`); no `Secure` (plain HTTP today) | unchanged + `Secure` once TLS lands (M1) |
| `ab_tenant` | gateway, legacy routing id | `HttpOnly`; `SameSite=Lax`; client-supplied value trusted if `^[a-f0-9]{16,64}$` — the pre-freeze credential flaw (A4#3) | removed: ignored everywhere, superseded by `ab_sid` |
| `dqa_session` | upstream DQA session | held server-side only; never proxied to the browser | unchanged |

**Current-vs-target deltas** (what WS-A/WS-B/WS-E must change to meet §8–§10): no server-minted `ab_sid` yet (client `ab_tenant` still trusted); teardown keeps tenant data dirs for 7 days and uses a 5 s SIGKILL grace; no `--import-roots`; no `GET /files`; no `POST /plugins/rescan`; admin gate checks login only, not role; plugin-sync currently abuses `PUT /{id}/enabled` as its rescan trigger (replaced by §2.27).
