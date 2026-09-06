# AnalysisBuddy HTTP API v1

> Status: **Active** (server-mode contract, M2). This English specification documents the REST + SSE surface exposed by `core/ab-server`, the headless HTTP service edition of AnalysisBuddy. Response bodies reuse the desktop command DTOs field-for-field; the fact source for those shapes is `core/ab-engine/src/commands/*` and the desktop contract `AnalysisBuddy-devdocs/deep-dive/ipc-ui.md` §1.0. This file introduces no DTO field that is absent there.
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
| Auth | Optional `Authorization: Bearer <token>` — see §7.2. |
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

Errors are always the uniform envelope of §4; 204 responses carry no body. Status codes are transport-level additions on top of the engine `IpcError.code` (§4) — clients SHOULD key off `code`, not the status.

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

### 2.3 POST /imports/upload — upload then import

`multipart/form-data`:

| Field | Type | Description |
|-------|------|-------------|
| `file` | binary | REQUIRED. File content. The part's filename (or the optional `filename` field) becomes the stored basename. |
| `filename` | text | OPTIONAL. Overrides the client-visible filename. |
| `overrides` | text (JSON) | OPTIONAL. Same shape as the `overrides` object of §2.2, keyed by the stored upload path. |

The server writes the bytes to `<OS temp dir>/ab-server-uploads/<pid>-<seq>-<nanos>/<basename>` (uniqueness comes from the directory; the basename preserves the client filename so `ImportResult.name` matches desktop semantics) and then runs the same job flow as §2.2 with that single path. Unknown multipart fields are ignored (forward compatibility). Missing `file` → 400.

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
| `session_io`, `state_io`, `internal`, `host_backpressure` | 500 | Server/engine-side failures. |
| Numeric codes (`-32700`…`-32602`-style) | 400 | JSON-RPC-style transport codes passed through. |
| Unknown code | 500 | Conservative fallback; new engine codes arrive before server releases in practice. |

The `code` field is the stable machine-readable identity; the status table is a convenience mapping and MAY be extended additively within `/api/v1`.

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

The server binds `127.0.0.1:8600` by default and is intended for loopback use (local tooling, embedded UIs). For remote deployment, front it with a reverse proxy that terminates TLS; use `--addr 0.0.0.0:8600` (or a specific interface) deliberately.

### 7.2 Authentication

`--token <token>` enables bearer auth: every request MUST carry `Authorization: Bearer <token>` except `GET /api/v1/health`. Failures get 401 with the uniform envelope (`code: "unauthorized"`). Token comparison is constant-time. Without `--token`, no auth is enforced — suitable only for loopback deployments. Deployments SHOULD additionally restrict at the network layer (bind address, firewall, proxy ACLs); bearer tokens are not a substitute for transport security.

### 7.3 Threat model

- **Plugins are arbitrary code.** They run as subprocesses with the server's OS privileges. Installing a plugin (§2.13) is therefore equivalent to granting code execution: restrict `/plugins/install` and `/plugins/{id}/update` to trusted operators (auth + network policy). Plugin ZIPs are validated for manifest/layout compliance, not for safety.
- **Uploads** land in the OS temp dir (`<temp>/ab-server-uploads/…`) and are imported like local files; each upload copy is removed after the import call. The request body limit is 64 MB.
- **Path confinement:** session save/load paths are confined to `--sessions-dir` (§2.18); uploads are confined to the server-managed upload dir; preset ids are charset-restricted. No endpoint accepts arbitrary server-side file reads.
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
