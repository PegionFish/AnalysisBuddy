# AnalysisBuddy Platform Evolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Every checkbox is a tracking item, not a suggestion.

**Goal:** Evolve the existing Windows analysis workbench into a shared-core Windows desktop and Linux single-node API platform with reliable manual module installation, structured analysis, and AI-ready JSON exports.

**Architecture:** Preserve the current Rust protocol/host/pipeline foundation. Extract transport-independent application services, add a Linux HTTP/SSE adapter and durable data root, then extend modules with optional analysis reports and compose only their published facts. The released Windows ZIP includes only `builtin-csv`; all other modules are manually installed from local ZIPs.

**Tech Stack:** Rust 2021, Tokio, Tauri 2, React/TypeScript/Vite, JSON-RPC over NDJSON, OpenAPI, SSE, SQLite, Docker/Compose, systemd, Python/.NET SDKs.

**Spec:** [Platform evolution specification](../architecture/2026-08-28-platform-evolution-spec.md)

## Global Constraints

- Windows desktop supports x86_64 and ARM64; Linux server is a first-class target.
- The first availability target is single-node resilience, not distributed clustering.
- Docker/Compose and systemd have the same data-root, health, recovery and acceptance semantics.
- Linux receives large inputs through two stages: resumable artifact upload, then idempotent ingestion creation.
- Server-side callers cannot supply arbitrary server filesystem paths.
- Plugin processes stay isolated; a plugin fault, timeout or budget breach must not terminate the host or silently lose records.
- Analysis rules are plugin-native in the first release; ad hoc input is data only, never executable code.
- Cross-module analysis reads only published facts, events, metrics and reports through an explicit DAG.
- `SessionAnalysisBundle` JSON is versioned, deterministic, size-bounded, traceable and redacted by default.
- The public Windows ZIP contains exactly `plugins/builtin-csv`; `demo-tool` is development/test-only.
- Modules are installed from local ZIP only. Remove automatic updates, downloads and background polling; retain user-triggered read-only version checks.
- Public code, CI and documentation must be self-contained and may not depend on untracked workspace material or module-specific private content.
- No agent pushes directly to `main`, force-pushes, changes another agent's worktree, or alters files outside its assigned allowlist.

---

## 0. Authority, preflight, and execution protocol

### 0.1 Canonical documents

This roadmap and the linked specification supersede conflicting future-facing parts of the historical `PLAN.md`. Existing protocol v1 is frozen until the contract tasks below explicitly version and approve extensions. Old task cards and audits remain evidence, not instructions to recreate completed work.

### 0.2 Mandatory preflight for every implementation agent

- [ ] Run `git -c safe.directory=<absolute-repository-path> status --short` before creating a worktree. The current repository may report dubious ownership; do not modify global Git configuration as a shortcut.
- [ ] Create an isolated worktree and branch named `agent/<wave>-<lane>-<topic>` before editing. Example: `git -c safe.directory=<repo> worktree add .worktrees/w1-api -b agent/w1-api-server`.
- [ ] Set an isolated Cargo target directory for Rust commands, for example `$env:CARGO_TARGET_DIR = "$PWD\.target-agent"`. Do not delete a shared `target/` directory to work around stale generated paths.
- [ ] Read the specification, this roadmap, the task's dependency contracts and the exact files named in the task before editing.
- [ ] Report branch/worktree, changed files, tests, evidence, unresolved risks and cross-domain requests in the handoff format in §8.

### 0.3 Shared-file lock

The integration lead exclusively owns the root `Cargo.toml`, `Cargo.lock`, root `.github/**`, root `.gitignore`, `PLAN.md`, `README.md`, and generated API client publication. A task that needs one of these files submits a precise patch request to the lead; it must not modify them itself.

---

## 1. Dependency graph and delivery waves

```text
W0 contracts + clean baseline
  └─ G0: schemas, state machines, ownership frozen
       ├─ W1-A transport-independent application service
       │    ├─ W1-B durable metadata/task recovery
       │    └─ W1-F desktop transport extraction
       ├─ W1-C host roots + transactional ZIP install
       │    └─ W1-E release package and CI boundary
       ├─ W1-D protocol/SDK/validator analysis extension
              └─ G1: unit/module gates
                   ├─ W2-A resumable upload + task creation
                   │    └─ W2-B HTTP/SSE/auth/query routes
                   ├─ W2-C module analyze + composition DAG
                   │    └─ W2-D analysis bundle/redaction
                   └─ W2-E Docker/systemd delivery
                          └─ G2: platform integration
                               ├─ W3 public-module optional analysis
                               ├─ W3 private-repository handoff kits
                               ├─ W3 black-box fault injection
                               └─ W3 performance/ARM64/release rehearsal
                                      └─ G3/G4: release candidate and independent acceptance
```

Within a wave, the lead should launch every non-overlapping lane concurrently. Strong models should additionally delegate read-only review, test-design, fixture-design and threat-model subtasks rather than serializing research. Write agents remain constrained by file ownership; read-only agents can run at the available concurrency ceiling.

---

## 2. Wave 0 — freeze contracts and make the baseline reproducible

### Task W0-01: Establish current-state and clean-build evidence

**Owner:** integration lead plus independent read-only build verifier
**Files:**

- Create: `docs/development/evidence/2026-08-28-baseline-build.md`
- Modify: none
- Test: existing workspace checks only

**Consumes:** current workspace and existing CI commands.
**Produces:** a reproducible command matrix, including the isolated target-dir workaround and a list of baseline failures proven to predate the change.

- [ ] Run `git -c safe.directory=<repo> status --short`; record that the worktree is clean or list unrelated user changes without modifying them.
- [ ] Run `cargo fmt --all --check` with an isolated `CARGO_TARGET_DIR`; record exit code and full first failure if any.
- [ ] Build UI with `npm --prefix ui run build`; run `npm --prefix ui run typecheck`, `npm --prefix ui run lint`, and `npm --prefix ui test -- --run`.
- [ ] Run focused Rust tests in independent commands: `cargo test -p ab-protocol`, `cargo test -p ab-host`, `cargo test -p ab-pipeline`, and `cargo test -p ab-app` after the UI build.
- [ ] Attempt `cargo test --workspace --all-targets` only with the isolated target directory; distinguish generated-artifact/path failures from source test failures.
- [ ] Write the exact command, shell, target path, result, elapsed time and failure classification into the evidence file.
- [ ] Commit only the evidence document: `docs: record isolated baseline build evidence`.

### Task W0-02: Freeze public contract ledger and versioning policy

**Owner:** contract lane
**Files:**

- Create: `docs/spec/api-v1-contract.md`
- Create: `docs/spec/analysis-report.schema.json`
- Create: `docs/spec/session-analysis-bundle.schema.json`
- Create: `docs/spec/examples/analysis-report-ok.json`
- Create: `docs/spec/examples/session-analysis-bundle-ai-safe.json`
- Modify: `docs/spec/protocol-v1.md`
- Modify: `docs/spec/rpc-messages.schema.json`
- Test: `core/ab-protocol/tests/serde_tests.rs` and new schema fixture test

**Consumes:** protocol v1 and the approved specification.
**Produces:** additive optional `analyze` capability, stable report/fact/bundle schemas, API error/event ledger, and explicit compatibility rules.

- [ ] Write failing fixture tests that load each new JSON example through `serde_json` and reject an unknown outcome, a missing `schema_version`, an executable `rule_input` field, and evidence without a producer.
- [ ] Extend protocol types with `AnalysisOutcome`, `AnalysisEvidence`, `AnalysisFact`, `AnalysisResult`, `AnalysisReport`, `AnalyzeRequest`, and `AnalyzeResult`; use `#[serde(rename_all = "snake_case")]` and deny unknown enum values.
- [ ] Add optional `analysis` capability negotiated during `initialize`; modules without it retain current behavior and are never sent `analyze`.
- [ ] Define `analyze` request limits exactly as `deadline_ms: u64` and `max_report_bytes: u64`; reject zero and values above host policy before spawning work.
- [ ] Define `SessionAnalysisBundle` with `schema_version`, `bundle_id`, `session_id`, `artifacts`, `modules`, `reports`, `composite_reports`, `omissions`, `redaction_profile`, and `generated_at_ms`.
- [ ] Add optional manifest metadata `repository` and `version_probe` with a URL, expected response schema and version field; document that it is only for an explicit read-only check and is never an installation source.
- [ ] Update `rpc-messages.schema.json`, protocol prose and examples together; no API or schema example may contain domain-specific module details.
- [ ] Run `cargo test -p ab-protocol` and a schema-validation command added by the task; commit `feat(protocol): add optional analysis report contract`.

### Task W0-03: Specify state machines, file ownership, and API fixtures

**Owner:** API/state-machine documentation lane
**Files:**

- Create: `docs/spec/server-state-machines.md`
- Create: `docs/spec/examples/ingestion-request-ok.json`
- Create: `docs/spec/examples/task-interrupted.json`
- Create: `docs/spec/examples/sse-resync-required.json`
- Create: `docs/development/platform-file-ownership.md`
- Test: JSON schema/fixture validation added in W0-02

**Consumes:** W0-02 type names.
**Produces:** exact upload, task, installation and SSE state transitions plus a future wave ownership matrix.

- [ ] Describe upload transitions `created → receiving → verifying → completed | rejected | expired` and identify each idempotent operation.
- [ ] Describe task transitions `queued → running → succeeded | failed | cancelled | interrupted`; list the only legal retry edge: `failed|cancelled|interrupted → queued` with a new task revision.
- [ ] Describe installation transitions `staging → validated → prewarmed → activated | rejected | rolled_back` and recovery rules for an unfinished journal.
- [ ] Describe SSE behavior for retained cursor, expired cursor, terminal events and revision de-duplication.
- [ ] Write an ownership table for all W1/W2 lanes; ensure no file appears in two simultaneous write lanes.
- [ ] Validate all examples against their schemas and commit `docs: freeze platform state machines and ownership`.

**Gate G0:** The integration lead compares W0-02 and W0-03 against the architecture specification, resolves every type mismatch, tags the contract baseline, and records the approved commit IDs. No W1 writer may change contract files without a new contract task.

---

## 3. Wave 1 — parallel foundations after G0

### Task W1-A: Extract transport-independent application services

**Owner:** application lane
**Files:**

- Create: `core/ab-application/Cargo.toml`
- Create: `core/ab-application/src/lib.rs`
- Create: `core/ab-application/src/service.rs`
- Create: `core/ab-application/src/error.rs`
- Create: `core/ab-application/tests/service_contract.rs`
- Modify: `core/ab-app/Cargo.toml`
- Modify: `core/ab-app/src/commands/{import,query,session,plugin}.rs`
- Modify: workspace member list through lead-owned patch request

**Consumes:** G0 contract types, current host and pipeline APIs.
**Produces:** `ApplicationService` methods `create_ingestion`, `get_task`, `cancel_task`, `retry_task`, `get_session`, `query_series`, `query_key_values`, `list_plugins`, and `export_analysis_bundle` with transport-free inputs and outputs.

- [ ] Write a failing `service_contract` test using in-memory/mock host and store adapters; assert a duplicate `idempotency_key` returns the original task ID without parsing twice.
- [ ] Define `ApplicationError { code: String, retryable: bool, message: String }`; map host/pipeline failures into stable codes without Tauri types.
- [ ] Move only orchestration from existing command modules into `ApplicationService`; keep file dialogs, Tauri emits and command serialization in `ab-app`.
- [ ] Add a second failing test that cancellation produces exactly one terminal task revision and does not remove the artifact reference.
- [ ] Implement the minimal service methods required by the tests, then adapt existing commands to call the service.
- [ ] Run `cargo test -p ab-application` and existing `cargo test -p ab-app`; request the root workspace patch from the lead; commit `refactor(application): extract transport-independent services`.

### Task W1-B: Add durable metadata and task recovery store

**Owner:** storage lane
**Files:**

- Create: `core/ab-application/src/storage/mod.rs`
- Create: `core/ab-application/src/storage/sqlite.rs`
- Create: `core/ab-application/src/storage/models.rs`
- Create: `core/ab-application/tests/task_recovery.rs`
- Modify: `core/ab-application/Cargo.toml`

**Consumes:** W0 state-machine ledger and W1-A task DTOs.
**Produces:** `MetadataStore` trait with `create_task`, `transition_task`, `find_idempotency_key`, `recover_interrupted_tasks`, and `transaction`.

- [ ] Write a failing SQLite temp-directory test that rejects illegal `succeeded → running` transition and accepts `running → interrupted` during recovery.
- [ ] Define a migration creating `artifacts`, `sessions`, `tasks`, `task_events`, and `idempotency_keys`, each with explicit primary key, revision and timestamps.
- [ ] Implement transaction-wrapped transition checks using the transitions listed in `server-state-machines.md`; return `task_conflict` on revision mismatch.
- [ ] Write a failing restart test: insert a running task, reopen the database, call `recover_interrupted_tasks`, then assert `interrupted` and a recovery event.
- [ ] Implement recovery and run `cargo test -p ab-application task_recovery`; commit `feat(storage): persist tasks and recover interruptions`.

### Task W1-C: Define root policy and transactional local ZIP activation

**Owner:** host-install lane
**Files:**

- Create: `core/ab-host/src/roots.rs`
- Create: `core/ab-host/src/install.rs`
- Create: `core/ab-host/tests/install_transaction.rs`
- Modify: `core/ab-host/src/lib.rs`
- Modify: `core/ab-host/src/discovery.rs`
- Modify: `core/ab-host/src/manifest.rs`

**Consumes:** G0 installation state machine and current manifest validation.
**Produces:** `PluginRoot`, `InstallRequest`, `InstallJournal`, `PackageVerifier`, `activate_version`, `rollback_version`, and discovery limited to active versions.

- [ ] Write a failing test that supplies a ZIP with `../escape.txt` and asserts the installer rejects it without writing outside staging.
- [ ] Write a failing test that seeds `versions/1.0.0` as active, makes prewarm of `1.1.0` fail, and asserts `active.json` still points to `1.0.0`.
- [ ] Define roots with `kind`, `path`, `precedence`, `writable`, `installable`, `removable`, `trust_policy`, and `metadata_policy`; do not include source-download logic.
- [ ] Define `PackageVerifier::verify(staging_dir, manifest) -> Result<(), InstallError>`; its first implementation checks safe layout, manifest schema, protocol compatibility and package checksums. Keep CLI behavior validation as a separate test/CI concern rather than spawning the validator from the GUI or server process.
- [ ] Implement staging extraction under the same root volume, manifest compatibility validation, `PackageVerifier`, prewarm handshake, version-directory placement, atomic active-pointer replacement, previous-pointer preservation, and journal writes.
- [ ] Make discovery ignore `staging`, incomplete journals and non-active version directories; preserve current priority/shadow behavior.
- [ ] Add a restart-recovery test for a journal interrupted between version placement and pointer activation.
- [ ] Run `cargo test -p ab-host install_transaction` and all host tests; commit `feat(host): activate local module ZIPs transactionally`.

### Task W1-D: Add optional analysis RPC, SDK support, and validator cases

**Owner:** protocol/SDK lane
**Files:**

- Modify: `core/ab-host/src/{rpc,session,health}.rs`
- Modify: `sdk/python/analysisbuddy/{plugin,context,transport}.py`
- Modify: `sdk/dotnet/{IPluginHandler.cs,Models.cs,PluginHost.cs}`
- Modify: `tools/plugin-validator/src/{behavior,harness,rules}.rs`
- Create: `tools/plugin-validator/tests/fixtures/bad-analysis-report/plugin.py`
- Create: `tools/plugin-validator/tests/fixtures/good-analysis-report/plugin.py`
- Modify: relevant SDK and validator tests

**Consumes:** W0-02 protocol extension.
**Produces:** optional `analyze` request/response behavior with report-size/deadline validation in all supported SDKs and validator.

- [ ] Write failing host test where a module lacking `analysis` capability is not sent `analyze`.
- [ ] Write failing host test where a report greater than `max_report_bytes` returns a stable error and marks only the analysis operation failed.
- [ ] Implement capability-gated RPC dispatch and deadline cancellation while preserving `parse` behavior.
- [ ] Add Python `on_analyze` and C# `OnAnalyzeAsync` defaults that return `not_applicable`; expose typed report/fact models.
- [ ] Add validator fixtures for malformed outcome, missing producer, overlarge report and successful minimal report.
- [ ] Run Rust host tests, Python SDK tests, .NET SDK tests and `cargo test --manifest-path tools/plugin-validator/Cargo.toml`; commit `feat(sdk): support optional analysis reports`.

### Task W1-E: Correct public release contents and remove update application

**Owner:** integration lead, assisted by release/desktop-plugin reviewer (shared release files)
**Files:**

- Modify: `core/ab-app/src/network/{mod.rs,update_fetcher.rs}`
- Modify: `core/ab-app/src/commands/{plugin.rs,plugin_manager.rs}`
- Modify: `ui/src/components/PluginManagerPage.tsx`
- Modify: `ui/src/components/PluginManagerPage.test.tsx`
- Modify: `scripts/{bundle-zip.ps1,verify-zip-manifest.ps1,arm64-smoke.ps1}`
- Modify: `README.md`
- Test: existing command/UI/package tests plus new ZIP assertion

**Consumes:** W1-C local installation interface.
**Produces:** no update installation path, user-triggered read-only version check, and a release ZIP containing only `builtin-csv`.

- [ ] Write a failing UI test that a module with a newer upstream version offers only a version notice and no update/download action.
- [ ] Replace any update-fetch-and-install code path with `check_plugin_version` returning `up_to_date`, `update_available`, `not_configured`, `unreachable`, or `invalid_metadata`; it must not write a plugin root.
- [ ] Reject non-HTTP(S) version probes, local file URLs, redirects to unsupported schemes, script execution and Git commands; only perform an explicit user-triggered metadata request.
- [ ] Remove demo module copying, vendored SDK copying and demo smoke requirements from the release ZIP script while retaining demo use in development E2E.
- [ ] Change manifest verification to assert `plugins/builtin-csv/**` exists and no second plugin directory exists.
- [ ] Update portable README text generated by the script and root README to say that external modules are manually installed local ZIPs.
- [ ] Run focused UI tests, `powershell -File scripts/verify-zip-manifest.ps1` against a generated test ZIP, and existing demo E2E separately; commit `feat(release): ship only builtin CSV and manual module installs`.

### Task W1-F: Preserve desktop behavior through the application adapter

**Owner:** desktop adapter lane
**Files:**

- Modify: `core/ab-app/src/{lib.rs,events.rs,host_bridge.rs,pipeline_bridge.rs}`
- Modify: `core/ab-app/src/commands/{import,query,session,plugin}.rs`
- Modify: `core/ab-app/tests/{host_bridge_test,pipeline_bridge_test,cancel_import_test,session_reopen_test}.rs`

**Consumes:** W1-A application services.
**Produces:** Tauri commands that map to the application DTOs and emit UI events from persisted task/session updates.

- [ ] Write a failing regression test that a desktop import and an `ApplicationService` ingestion expose identical task terminal status and error code.
- [ ] Replace direct pipeline/host mutations in each command with the service call; preserve current Tauri command names until a deliberate API deprecation task.
- [ ] Write a failing test that the bridge blocks/backpressures rather than discarding a record batch when its subscriber is slow.
- [ ] Implement bounded await/explicit failure semantics and surface a stable task failure code; do not use `try_send` for loss-tolerant data batches.
- [ ] Run `cargo test -p ab-app`; commit `refactor(desktop): route commands through application services`.

**Gate G1:** The lead integrates W1 branches in dependency order, runs per-crate/SDK/UI checks, validates that no root/shared-file conflict was smuggled into a branch, and records the exact baseline delta.

---

## 4. Wave 2 — server, durable ingestion, composition, and operational delivery

### Task W2-A: Implement artifact upload and ingestion creation

**Owner:** server-ingestion lane
**Files:**

- Create: `core/ab-server/Cargo.toml`
- Create: `core/ab-server/src/{lib.rs,config.rs,uploads.rs,ingestions.rs}`
- Create: `core/ab-server/tests/upload_resume.rs`
- Modify: `core/ab-application/src/{service.rs,storage/models.rs,storage/sqlite.rs}`
- Modify: workspace member list through lead-owned patch request

**Consumes:** W1-A application service, W1-B metadata store, W0 API fixtures.
**Produces:** resumable upload handles, content-hash promotion to artifact, and idempotent ingestion creation.

- [ ] Write a failing test that uploads chunk 0, restarts the server/store, uploads the remaining chunk, completes it, and obtains an artifact whose SHA-256 equals the source fixture.
- [ ] Write a failing test that the same `idempotency_key` and ingestion body returns the original `task_id`, while the same key with a different canonical body returns `idempotency_conflict`.
- [ ] Define `UploadId`, `ArtifactId`, `IngestionId`, `CreateIngestionRequest`, `UploadChunk`, and `ArtifactMetadata` in transport-free application types.
- [ ] Implement chunk offset checks, per-chunk maximum, total-size budget, rolling/final SHA-256 verification, atomic promotion from staging, and TTL metadata for unlinked artifacts.
- [ ] Implement `create_ingestion` so only a completed artifact can enter a task; reject arbitrary paths and absent/invalid source metadata.
- [ ] Run `cargo test -p ab-server upload_resume`; commit `feat(server): accept resumable artifacts and idempotent ingestions`.

### Task W2-B: Implement OpenAPI routes, snapshots, SSE, and API Key policy

**Owner:** HTTP/SSE lane
**Files:**

- Create: `core/ab-api/Cargo.toml`
- Create: `core/ab-api/src/{lib.rs,models.rs,errors.rs,events.rs,openapi.rs}`
- Create: `core/ab-server/src/{router.rs,handlers.rs,sse.rs,auth.rs,health.rs}`
- Create: `core/ab-server/tests/{api_contract.rs,sse_resume.rs,auth_policy.rs}`
- Modify: `docs/spec/api-v1-contract.md`

**Consumes:** W0 contract files, W1-A/W1-B/W2-A services.
**Produces:** `/health`, `/api/v1` route implementation, generated OpenAPI document, SSE resume and explicit listener/auth policy.

- [ ] Write a failing contract test for `GET /health/live` and `GET /health/ready`: live returns 200 while ready returns 503 before storage recovery completes.
- [ ] Write a failing API test that LAN binding without a configured API Key refuses startup, while loopback binding allows local trusted startup.
- [ ] Write a failing SSE test that reconnects with a retained `Last-Event-ID` and receives only subsequent events; write another that receives `resync_required` for an expired ID.
- [ ] Define versioned request/response DTOs in `ab-api`; map every `ApplicationError` to a documented HTTP status and machine code.
- [ ] Implement routes for uploads, ingestions, tasks, sessions, queries, module list/install/verify/rollback/version check, bundle export and events. Routes must call `ApplicationService`, never host/pipeline internals directly.
- [ ] Implement an in-memory bounded event broker backed by persistent task/session snapshots; important state transitions are persisted before publication.
- [ ] Generate and validate the OpenAPI document in test; run `cargo test -p ab-server`; commit `feat(api): expose versioned server routes and resumable SSE`.

### Task W2-C: Add analysis execution, facts, and composition DAG

**Owner:** analysis-composition lane
**Files:**

- Create: `core/ab-application/src/analysis/{mod.rs,executor.rs,plan.rs,report_store.rs}`
- Create: `core/ab-application/tests/{analysis_execution.rs,composition_plan.rs}`
- Modify: `core/ab-application/src/{service.rs,storage/models.rs,storage/sqlite.rs}`
- Modify: `core/ab-host/src/session.rs`

**Consumes:** W0-02 report contract and W1-D host RPC.
**Produces:** capability-gated module analysis and explicit `AnalysisPlan` DAG evaluation based only on published inputs.

- [ ] Write a failing test that a module without analysis capability finishes parsing and yields no report rather than a task failure.
- [ ] Write a failing test that an analysis report references an unpublished fact and is rejected with `analysis_evidence_invalid`.
- [ ] Write a failing DAG test where an upstream node fails and its dependent is recorded as `blocked`, while an independent branch succeeds.
- [ ] Persist reports/facts with producer module version, rule revision, artifact reference, task revision and timestamp.
- [ ] Implement topological plan validation: reject cycles, unknown input references, duplicate node IDs and incompatible pinned module versions before execution.
- [ ] Implement analysis execution using host limits and report byte budget. Store `partial`, `blocked` and `not_applicable` explicitly; never coerce them to `pass`.
- [ ] Run focused application tests; commit `feat(analysis): execute module reports and composition plans`.

### Task W2-D: Export deterministic, redacted SessionAnalysisBundle JSON

**Owner:** bundle/redaction lane
**Files:**

- Create: `core/ab-application/src/export/{mod.rs,bundle.rs,redaction.rs,canonical_json.rs}`
- Create: `core/ab-application/tests/{bundle_export.rs,redaction.rs}`
- Modify: `core/ab-application/src/service.rs`
- Modify: `core/ab-api/src/models.rs`

**Consumes:** W0-02 bundle schema and W2-C report persistence.
**Produces:** profile-based bundle export used by desktop/API consumers and later AI adapters.

- [ ] Write a failing test that exporting the same immutable session twice yields byte-identical canonical JSON after excluding the explicitly documented generation timestamp field.
- [ ] Write a failing test that `ai-safe` omits raw lines, absolute paths, complete context, stderr and fields marked restricted, while retaining omission reasons.
- [ ] Define profiles `summary`, `review`, `automation`, `ai_safe` and an explicit maximum exported byte budget. Return `bundle_too_large` with a suggested narrower scope rather than truncating a JSON document.
- [ ] Implement canonical object-key ordering, stable array ordering by declared IDs, SHA-256 bundle ID calculation, and traceable producer/version references.
- [ ] Add an API contract test that validates the response against `session-analysis-bundle.schema.json`.
- [ ] Run bundle and API tests; commit `feat(export): provide redacted deterministic analysis bundles`.

### Task W2-E: Add Docker and systemd operational delivery

**Owner:** operations lane
**Files:**

- Create: `deploy/docker/Dockerfile`
- Create: `deploy/docker/compose.yaml`
- Create: `deploy/systemd/analysisbuddy-server.service`
- Create: `deploy/systemd/analysisbuddy-server.env.example`
- Create: `docs/operations/linux-deployment.md`
- Create: `docs/operations/backup-and-recovery.md`
- Create: `tests/operations/docker-smoke.ps1`
- Create: `tests/operations/systemd-contract.md`

**Consumes:** W2-A/B server binary/config and data-root contract.
**Produces:** reproducible container and systemd reference delivery, health/restart/backup evidence.

- [ ] Write a Docker smoke test that starts with an empty named volume, waits for readiness, creates an artifact/task, restarts the container, then verifies task/session metadata remains queryable.
- [ ] Build the image with a non-root runtime user and an explicit `DATA_DIR`; ensure image build context excludes `.worktrees`, `target`, `node_modules`, external directories and user modules.
- [ ] Configure Compose restart policy, named volume, loopback-only host port publication by default, liveness/readiness health check, and no default external update endpoint.
- [ ] Write a systemd unit with `User=analysisbuddy`, `StateDirectory=analysisbuddy`, `Restart=on-failure`, bounded restart burst, explicit `TimeoutStopSec`, and environment-file loading.
- [ ] Document backup as an integrity-checked SQLite snapshot plus session/artifact/module pointer manifest; document restore into a stopped service and readiness verification.
- [ ] Run Docker smoke where Docker is available; otherwise record a reproducible manual prerequisite rather than claiming a pass. Validate unit syntax/review contract and commit `feat(ops): deliver docker and systemd server references`.

**Gate G2:** The lead runs one complete artifact-to-bundle flow through HTTP, validates an install rollback after forced prewarm failure, replays an SSE reconnect, and verifies the same session query result through the desktop application service adapter.

---

## 5. Wave 3 — module adoption, UI, independent validation, and release rehearsal

### Task W3-A: Make desktop/UI consume stable analysis and module-version state

**Owner:** desktop UI lane
**Files:**

- Modify: `ui/src/ipc/{types.ts,ipc.ts,events.ts,mock.ts}`
- Create: `ui/src/components/AnalysisSummary.tsx`
- Create: `ui/src/components/AnalysisSummary.test.tsx`
- Modify: `ui/src/components/{AppShell.tsx,PluginManagerPage.tsx}`
- Modify: `ui/src/state/session.ts`

**Consumes:** W2-C/W2-D application/API DTOs.
**Produces:** desktop rendering of analysis summaries, partial/blocked states, version notices and manual-install guidance.

- [ ] Write a failing component test for `warning`, `fail`, `partial`, `blocked`, and `not_applicable` report summaries; no rich plugin HTML is rendered.
- [ ] Add transport DTOs and mock fixtures from public analysis schemas only; do not add a static directory of external module-specific onboarding data.
- [ ] Render rule/module version and evidence navigation using plain text and IDs. For `update_available`, render manual-download guidance without an update action.
- [ ] Add accessibility tests for keyboard focus and state announcements; run UI test, typecheck, lint and i18n check; commit `feat(ui): display structured analysis summaries`.

### Task W3-B: Public module optional analysis adoption

**Owner:** one independent agent per public module repository
**Files:** module-local `main.py`, parser/rule code, `plugin.json`, tests, fixtures, README in each repository only.

**Consumes:** published G0 protocol/SDK tag and W1-D validator.
**Produces:** optional fast analysis for modules where a public, testable rule is appropriate.

- [ ] Pin the consumed SDK/protocol compatibility range in the module repository and add a fixture-based failing analysis test.
- [ ] Implement `on_analyze` solely from that module's parsed data; return `not_applicable` when the input cannot support the rule.
- [ ] Emit report/fact evidence with metric/time references, not raw unbounded input lines.
- [ ] Run that repository's unit tests and `plugin check --behavior` against its public fixture; create an independent module release tag only after the host compatibility test passes.
- [ ] Do not modify the public core from this task and do not add any external module to the core release package.

### Task W3-C: Prepare external-module and API-client handoff kits

**Owner:** documentation/template lane; private implementation occurs only in controlled repositories
**Files:**

- Create: `docs/developer-guide/10-analysis-capability.md`
- Create: `docs/developer-guide/11-local-module-package.md`
- Create: `sdk/python/examples/analysis-plugin/README.md`
- Create: `sdk/python/examples/analysis-plugin/main.py`
- Create: `sdk/python/examples/analysis-plugin/plugin.json`
- Create: `sdk/python/examples/analysis-plugin/tests/test_analysis.py`
- Modify: `docs/developer-guide/README.md`

**Consumes:** G0 schemas, W1-D SDK API, W1-C local package format, W2-D bundle profiles.
**Produces:** public, domain-neutral implementation guide and testable example; private repositories receive no content through public CI.

- [ ] Write an example test that calls `on_analyze` with a generic threshold data entry and asserts a typed `AnalysisReport` with evidence.
- [ ] Implement only generic examples and generic field names; no domain tooling, local paths, deployment endpoints or internal workflow terms may appear.
- [ ] Document local ZIP packaging, compatibility declaration, `analyze` capability, data-only ad hoc input, evidence, bundle redaction and manual version check semantics.
- [ ] Validate the example with the validator and SDK tests; commit `docs(sdk): document analysis-capable local modules`.

### Task W3-D: Bootstrap controlled repositories and the separate WebConsole

**Owner:** controlled-repository lead; this task runs outside the public main worktree
**Files:**

- Create in every controlled module repository: `.gitignore`, `README.md`, `plugin.json`, test command entry point, and local release-note convention
- Create in the separate WebConsole repository: `.gitignore`, `README.md`, OpenAPI client generation script, lockfile, and local development instructions
- Create in the controlled deployment repository, if used: environment example, artifact-catalog format, and offline release procedure
- Test: local Git remote audit, module validator, WebConsole generated-client typecheck

**Consumes:** published protocol/SDK/API contract tags and the neutral handoff kit from W3-C.
**Produces:** one local Git repository per controlled module, a separate local Git repository for the WebConsole, and no dependency from the public main repository back to them.

- [ ] Initialize each controlled module directory as its own local Git repository. Confirm `git remote -v` is empty before the first commit; do not add a public remote.
- [ ] Add ignores for virtual environments, build output, raw logs, generated reports, local configuration, secrets and artifact directories. Do not use a parent workspace repository to track any controlled-module file.
- [ ] Create each module's minimal manifest, parser/rule test entry point, compatibility declaration and local release-note template from the public SDK contract; keep all format-specific fields and fixtures inside that repository.
- [ ] Initialize the WebConsole as an independent local repository. Generate its TypeScript API client only from the versioned public OpenAPI document; do not copy or fork the desktop UI.
- [ ] Add an offline CI/release procedure that validates module ZIPs, tests with local fixtures and emits local artifacts without contacting public CI or a public package registry.
- [ ] Record the public contract version consumed by each repository and make the initial local commits. When an internal Git service becomes available, add only the approved internal remote and push existing branches/tags without rewriting history.
- [ ] Run the remote audit, local tests and generated-client typecheck; preserve evidence in the corresponding controlled repository rather than in the public main repository.

### Task W3-E: Independent black-box fault-injection and public-package audit

**Owner:** verification lane; must not be an implementer from W1/W2
**Files:**

- Create: `tests/e2e/tests/e2e_server_resilience.rs`
- Create: `tests/e2e/tests/e2e_zip_activation.rs`
- Create: `tests/e2e/tests/e2e_analysis_bundle.rs`
- Create: `tests/scripts/assert-public-package.ps1`
- Create: `docs/development/evidence/2026-08-28-independent-validation.md`

**Consumes:** merged W2 system behavior.
**Produces:** black-box evidence for failure containment, bundle redaction and release contents.

- [ ] Write an E2E test that kills a module process during analysis and asserts the server remains ready, the task is terminal/interrupted, and an independent task can finish.
- [ ] Write an E2E test that forces a failed ZIP prewarm and asserts the previously active module remains selected after process restart.
- [ ] Write an E2E test that exports an `ai_safe` bundle and asserts raw input fixture content and absolute paths are absent.
- [ ] Write the package audit script to enumerate ZIP entries, require `plugins/builtin-csv/`, reject `plugins/demo-tool/`, and reject every second first-level plugin directory.
- [ ] Run all three tests and package audit from a clean temporary output directory; record commands, commit IDs, failures injected and outcomes in the evidence document.
- [ ] Commit `test(e2e): verify resilient server and public package boundary`.

### Task W3-F: Performance, ARM64, and operational rehearsal

**Owner:** performance/release verifier
**Files:**

- Create: `tests/perf/tests/server_upload_budget.rs`
- Create: `tests/perf/tests/analysis_bundle_budget.rs`
- Modify: `tests/scripts/perf-smoke.yml`
- Modify: `docs/arm64-smoke-checklist.md`
- Modify: `docs/release-acceptance.md`

**Consumes:** W2 artifact/upload/bundle behavior and W1-E packaging.
**Produces:** bounded large-input and export performance evidence plus explicit ARM64/server release signoff.

- [ ] Write a perf test with generated fixture data that asserts upload/ingestion respects configured byte and record budgets and produces a stable `resource_limit_exceeded` rather than process termination.
- [ ] Write a bundle test that verifies a large session returns a bounded summary or `bundle_too_large`, never malformed/truncated JSON.
- [ ] Update ARM64 checklist to assert the release ZIP contains only builtin CSV and to distinguish cross-compiled evidence from native execution evidence.
- [ ] Add server Docker/systemd recovery rehearsal rows to release acceptance; run relevant perf commands and record actual thresholds rather than invented numbers.
- [ ] Commit `test(perf): enforce server and bundle resource budgets`.

**Gate G3:** An independent verification agent runs W3-E and W3-F in a clean worktree, then the lead resolves any release blockers without changing test expectations to hide failures.

---

## 6. Wave 4 — integration, release candidate, and handoff

### Task W4-01: Integrate protected changes and update authoritative documentation

**Owner:** integration lead only
**Files:**

- Modify: `PLAN.md`
- Modify: `README.md`
- Modify: `docs/developer-guide/README.md`
- Modify: `.github/workflows/{ci,e2e-suite,release}.yml`
- Create: `docs/development/2026-08-28-platform-release-checklist.md`

**Consumes:** all accepted Wave 1–3 branch commits and independent evidence.
**Produces:** one authoritative index, CI gates, release checklist and merged release candidate.

- [ ] Merge branches in the dependency order from §1; after each merge run the narrow affected test suite before merging the next branch.
- [ ] Replace outdated claims that a release contains demo modules or performs online module updates; state the actual manual ZIP behavior.
- [ ] Make CI build demo only where E2E needs it, not where release packaging runs; make release workflow invoke the public package audit script.
- [ ] Link this specification and roadmap from the authoritative plan index; do not link to untracked local materials.
- [ ] Run UI build/test/lint/typecheck, Rust format/clippy/workspace tests, SDK tests, validator tests, E2E, package audit and release smoke.
- [ ] Commit documentation/workflow integration with a single release-candidate commit only after all checks are green.

### Task W4-02: Execute release-candidate recovery drill

**Owner:** independent operations/release agent
**Files:**

- Create: `docs/development/evidence/2026-08-28-release-candidate-drill.md`
- Modify: none unless evidence reveals a reproducible documentation error

**Consumes:** W4-01 release candidate ZIP/image/unit.
**Produces:** final evidence of clean installation, manual external ZIP behavior, restart recovery and no-background-update behavior.

- [ ] Use a fresh temporary application/data directory and install the release ZIP; verify the only bundled module is builtin CSV.
- [ ] Install a validator-approved external test ZIP manually, verify it, force a failed candidate install, and verify rollback leaves the previous healthy version active.
- [ ] Trigger the read-only version check against a controlled metadata endpoint; assert no additional module files are created and no installation task runs.
- [ ] Start the Linux service deployment, complete artifact upload → ingestion → analysis bundle export, restart it, and query the same session again.
- [ ] Disconnect/reconnect an SSE client with a retained cursor and verify either correct replay or explicit resync.
- [ ] Record commands, package hashes, image/binary versions and every observed result; report any deviation as release-blocking.

### Task W4-03: Hand off to subsequent agents

**Owner:** integration lead
**Files:**

- Modify: this roadmap checkbox state
- Create: `docs/development/2026-08-28-next-agent-brief.md`

**Consumes:** final evidence and remaining unchecked items.
**Produces:** a self-contained next-agent entry point.

- [ ] Mark completed tasks with evidence links; leave unfinished tasks unchecked with their exact current blocker, not a vague status.
- [ ] List the current contract tag, supported API/protocol versions, last known good release candidate, and commands that reproduced the green baseline.
- [ ] List every active branch/worktree and confirm no private repository, external package, generated target directory or local configuration is required to understand the public handoff.
- [ ] State that future agents must begin at §0.2, read the specification, and obey single-writer ownership before opening implementation subtasks.
- [ ] Commit `docs: hand off platform evolution roadmap`.

**Gate G4:** The integration lead approves a release candidate only after W4-02 evidence is complete, the public package audit proves that `builtin-csv` is the sole bundled module, and the next-agent brief identifies every remaining unchecked item and its owner. A failed drill, an untracked dependency, or missing rollback evidence blocks release.

---

## 7. Prioritized work queue

### P0 — blocks implementation

- [ ] W0-01 clean baseline evidence
- [ ] W0-02 contract ledger and JSON schemas
- [ ] W0-03 state machines and ownership matrix
- [ ] W1-A application-service extraction
- [ ] W1-B durable task metadata/recovery
- [ ] W1-C transactional local ZIP activation
- [ ] W1-E only-builtin release package and no online update application

### P1 — first usable Linux/API platform

- [ ] W1-D optional analysis protocol/SDK/validator
- [ ] W1-F desktop adapter/backpressure
- [ ] W2-A resumable uploads and idempotent ingestion
- [ ] W2-B HTTP/OpenAPI/SSE/API Key routes
- [ ] W2-C module analysis and composition DAG
- [ ] W2-D deterministic redacted analysis bundle
- [ ] W2-E Docker and systemd delivery

### P2 — workflow adoption and release hardening

- [ ] W3-A desktop analysis summaries
- [ ] W3-B optional public-module analysis releases
- [ ] W3-C neutral analysis-module handoff kit
- [ ] W3-D controlled repository and WebConsole bootstrap
- [ ] W3-E independent fault injection/package audit
- [ ] W3-F performance/ARM64/operations rehearsal
- [ ] W4-01 protected integration and CI documentation
- [ ] W4-02 independent release-candidate drill
- [ ] W4-03 next-agent handoff

### Deferred deliberately

- [ ] Multi-node active-passive or active-active deployment
- [ ] Automatic module download, install, update or background polling
- [ ] Remote URL log ingestion, real-time tail and arbitrary server path import
- [ ] WebConsole editing of arbitrary executable rules
- [ ] Core-owned cloud model integration, automatic external AI upload or model-specific prompts
- [ ] General user multi-tenancy, SSO and granular authorization

---

## 8. Agent handoff contract and escalation rules

Every agent sends this exact completion report to the integration lead:

```text
AGENT: <lane/task>
STATUS: done | partial | blocked
BRANCH/WORKTREE: <absolute path and branch>
BASE COMMIT: <sha>
CHANGED FILES: <one path per line>
CONTRACTS CONSUMED: <schema/API/protocol versions>
CHECKS: <command => exit code => short result>
EVIDENCE: <test report, fixture, screenshot only if non-sensitive>
CROSS-DOMAIN REQUESTS: <exact file + requested edit + reason>
RISKS/ROLLBACK: <failure mode and reversal>
```

Escalate instead of editing when any of the following happens:

- a task needs to alter a frozen JSON schema, API type, protocol method, state transition, shared workflow or root configuration;
- a task needs a file owned by another active lane;
- a test fixture would require non-public content in the public repository;
- a behavior would download, execute, or automatically install a module from a network location;
- a change would permit arbitrary code in `context`, `rule_input`, a bundle, plugin metadata or AI output;
- a result cannot be reproduced from the task's worktree and declared inputs.

The lead may use as many read-only/reviewer agents as the environment allows. Only the lead resolves conflicts, integrates branches, updates shared files, marks gates complete, and approves any release candidate.
