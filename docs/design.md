# Notary: design and build plan (approved Nov 4, 2026)

## Context
Ritesh is rebuilding Agora for the Flip the Switch Hackathon, which is powered by sandbox-quantum/switch. Submissions close **Nov 7, 23:59 IST**. The aim is to submit by **Nov 7, 20:00 IST**.

They chose:
- **Notary** for Track 01, Best Business Workflow.
- A solo build with Claude.
- A Track 03 core PR (#801) worked in parallel.
- The Code with Wispr, Best Blog and #HowISwitch side prizes.

Nine hours of Nov 4 were lost to a usage limit and a session restart. This plan starts at **Nov 4, 23:00 IST**.

**What Notary is:**
- A Switch work-order desk. People file requests in a hub room.
- A Claude Code builder and an OpenCode reviewer work in a separate work room.
- A custom Rust **Notary** agent checks every claim they make against independent evidence before anything reaches the people. That evidence is git, its own sandboxed test run, and Switch's tool-activity log.
- Contradicted reports go back to the builder, at most twice.
- People in the hub see only checked claims, an approval card, and a signed, hash-chained receipt that anyone can verify offline.

**Where the parts come from:**
- agora-rs's trust core (the ledger and signing), rebuilt around the patterns in the user's `~/Documents/rea-blueprint`: content-hash evidence IDs, separate authority/confidence/coverage fields, a closed set of error tags, no silent fallback, owned process cleanup, and evaluation without an LLM.

**Judging and rules this plan is built around:**
- Weights: Use of Switch 30%, Technical 25% (judges clone and reproduce), Creativity 25%, Presentation 20%.
- Reusing earlier code is allowed if the README discloses it. The Switch setup and workflow must be built Nov 4–7.

**Sources:** research reports (Switch protocol and deploy, agora-rs domain, #801) were produced this session from shallow clones of switch@aa71e82 and agora-rs, and checked against the `switch-v0.31.0` tag.

---

## 1. Success criteria (verifiable)
1. `make demo-offline` runs from a clean clone with no model keys. It needs Docker or podman plus Rust. It brings up the pinned Switch and Mattermost and runs 7 scripted scenarios. It prints the expected verdict codes and exits 0, and the Notary's posts are visible in Mattermost.
2. **Live run:** sam files WO-17 in `#acme-desk`. Claude Code fixes `acme-invoices` in `#acme-work`. The Notary bounces at least one bad claim with codes and evidence IDs. OpenCode reviews. priya approves through the card, and sam's attempt to approve is refused. A signed receipt is posted, and `notary-verify receipt.json` prints OK. Editing one byte makes it FAIL.
3. `cargo test` passes in CI, covering the judge fixtures, the ledger, signing, receipt verification and canonical JSON.
4. The README has a "would it stop working without Switch" table and a prior-work disclosure. The video runs under 3 minutes. The blog, three #HowISwitch posts and the Wispr Insights screenshot are done.
5. The #801 PR is opened against sandbox-quantum/switch with tests, a real-CLI summary and a video, by Nov 7.

## 2. Architecture

**Switch (unmodified, pinned at `SWITCH_VERSION=0.31.0`):**
- switch-core and Mattermost 11.9.0 via the published standalone compose, profile `collab`.
- The UI and API are both on `:8000`, Mattermost on `:8065`.
- Switch Console 0.40.1 (rpm) runs the Claude Code builder and the OpenCode reviewer.

**Switch primitives we use:**
- Rooms, a group and a link.
- Room aliases, used for routing.
- Room documents (only the creator can edit them).
- Addressing policies, used for authorization.
- Approval cards.
- The session tool-activity log.
- The append-only room log, used as a witness.
- The v1 template, which installs everything.
- Agents from several providers: Claude Code, OpenCode, and our custom Rust agent.

**Ours:** the `notary` Rust agent, plus the template, scripts, sandbox image, demo repo and scripted test agents.

**Rooms (template `templates/notary-desk.template.yaml`, v1, a group of two linked rooms):**
- `acme-desk` (hub): Mattermost users sam (requester) and priya (approver), plus `notary`. **The builder and reviewer are never members**, so unchecked words can't reach people. The gate is built from room topology.
- `acme-work` (private): `acme-builder` (Claude Code), `acme-reviewer` (OpenCode), `notary`.
  - Aliases `builder` and `reviewer` point at those agents.
  - Template docs: **Desk Policy** (human text plus a fenced `notary-policy/1` JSON block holding approvers, the acceptance argv, protected paths and max bounces) and **Reporting Contract** (the `notary-claims/1` format).
  - The Notary reads the policy from Switch at intake and records its content hash in the ledger, so the policy lives in the room.

**Data flow for one work order:**
1. sam posts `@notary new Globex invoices over $10k show wrong tax` in the hub. The Notary receives it over WebSocket (`filter=all`).
2. The Notary does three things:
   - Fetches the repo and records `base` = the `main` SHA.
   - Loads Desk Policy (`connect_to_room` → `documents`, then `load_internal_documents`).
   - Creates a room doc `WO-17` in the work room (`create_room_document`).
   It then addresses `builder` with `send_targeted_message(target_names=["builder"])` and posts a one-line status to the hub.
3. Claude Code works in its Console checkout, pushes branch `wo-17` to the demo repo, and reports to `@notary` with a ```` ```notary-claims ```` JSON block.
4. The Notary collects evidence:
   - **git:** fetch into its mirror; `cat-file`; `merge-base --is-ancestor`; `diff --name-only base...sha`.
   - **notary_run:** a podman sandbox re-runs the Desk Policy argv at the SHA.
   - **switch_host:** the builder's tool-activity rows from `GET /gateway/chats/{work}/activity`. Used as corroboration only.
   Each item is sealed as `ev_<sha256(canonical JSON)>` and appended to the hash-chained ledger. The Notary then judges every claim as backed, contradicted or unverified, with a code.
5. Anything contradicted, or a finding the builder can fix, gets a ```` ```notary-bounce ```` block sent to `builder`: codes, retryable, remediation and `ev_` IDs. This happens at most 2 times; after that the code is `revision_limit` and a person decides.
6. Once the builder's report is clean, the Notary addresses `reviewer`. The reviewer's claims (`reviewed_diff`, `check_ran`) are judged the same way. Anything unverified becomes a flag on the card.
7. **Hub:** one root line, a "WO-17 Receipt" doc and an approval card (`POST /agent-sessions/wo-17/approvals`, `room_id=hub`).
   - The outcome comes from the `approval_outcome` frame, or from `GET /agent-sessions/approvals/outcomes` after a reconnect, followed by `/delivered`.
   - The Notary checks `answered_by` against the policy's approvers. Switch policy can't separate approvers from requesters in the same room, so this check is ours.
   - If a non-approver answers, the Notary posts the refusal and opens a new card.
8. **On approval:**
   - Append a `decision` record.
   - Sign the ledger head with ed25519.
   - Post the head hash in the hub (room-log witness).
   - Upload `receipt-wo-17.json` through `POST /agents/{id}/rooms/{hub}/media`.
   - Mark the WO approved for shipping. Merging stays the person's job.

**Notary commands (in the hub):**
- `new <text>`
- `status`
- `show <WO|claim|ev_prefix> [page N]` (20 lines per page; an ambiguous prefix lists candidates rather than guessing)
- `approve` / `hold` as a text fallback, used only when `NOTARY_APPROVAL=text`

**Switch protocol facts the code must respect** (from the verify reports):
- **Registration:**
  - Register with `POST /agents`, `integration_profile.connection_model="always_on"`, using the **admin's personal API key** (`POST /gateway/api-keys`). Never use the bootstrap token: its owner can't be addressed by people, and the owner-agents policy refuses it.
  - Save the key to a 0600 file. On a 409, reuse the saved key.
- **WebSocket:** `GET /agents/{id}/connection/ws?connection_id=<persisted uuid>&scope=all&filter=all&protocol=9&protocol_accepts=4&start_at=<persisted position|head>`.
  - Answer `ping` from the reader task.
  - Handle `gap` (re-read the room), `evicted` (`taken_over` means stop), `approval_outcome` (dedupe), and back-off from 1 to 30 s.
  - Dedupe on `(room_id, message_id)`.
  - A long poll leaves approval cards unanswerable, which is why the WebSocket is the only transport.
- **Room-bound ops:** call `connect_to_room(room)` behind a mutex first, then `POST /agents/{id}/ops/{name}` with `X-Switch-Connection-Id`.
- **Mentions:**
  - Only the name or an alias wakes an agent, not the display name.
  - Backticks don't defuse a mention. Write `@​name` or the bare name.
  - Never write `@word:`.
  - Keep posts under 4000 characters (the Mattermost limit). Put verdicts at the root, not in threads.
- **Unsupported, so don't build on them:** the tasks protocol (removed in 0.31.0), message edits, a scheduler, history replay, and template `packages:` (HEAD only).
- **Template docs** have no creator, so the Notary can't edit them. The Notary creates its own docs (WO-N, receipts, Desk Status).

## 3. Repository
New public repo **`Ritesh-Root/notary`** at `/home/ritesh/Documents/Flip the Switch Hackathon/notary` (MIT). A second small public repo, **`Ritesh-Root/acme-invoices`**, holds the demo repo. Console's `repo:` accepts only https/ssh URLs.

```
notary/
  Cargo.toml, clippy.toml        # from agora-rs: edition 2024, rust 1.99, unsafe forbid, clippy all=deny, deps pinned with =
  crates/notary-core/            # pure, no I/O except SQLite
    ids.rs        canonical JSON (serde_json Value, sorted keys, no floats/ints >2^53) + content_id("ev"|"vd")
    evidence.rs   Authority{git,notary_run,switch_host,self_reported,human} Confidence{observed,derived,inferred}
                  Coverage{complete|partial{limitations}|unknown{reason}}, Body, Evidence::seal/authenticate
    errors.rs     closed Code enum (~21 tags) + Code::meta() {retryable, fixer, remediation}; NotaryError
    claims.rs     notary-claims/1 parser (strict JSON, deny_unknown_fields, caps: block ≤16 KiB, ≤40 claims)
    judge.rs      pure judge(report, policy, &[Evidence]) -> Audit; next() -> Bounce | ReadyForApproval | Escalate
    ledger.rs     LIFTED from agora-rs ledger.rs (chain, verify_chain, verify_head); salt/forget dropped; UNIQUE(run, record)
    signing.rs    LIFTED from agora-rs signing.rs (ed25519 verify_strict, 0600 key); domain "notary signed head v1"
    render.rs     bounce block, hub line, receipt doc, show pages (code-written text, never builder prose)
    receipt.rs    ReceiptBundle{head, entries, records}; verify = re-hash records → chain → head/count → signature
    bin/notary-verify.rs
  crates/notary/                 # the agent binary
    config.rs     env parsed once; invalid value → configuration_invalid (no default fallbacks for engine/transport/approval mode)
    switch/       ws.rs (stream + ping task), http.rs (message, ops, media, approvals, outcomes), gateway.rs (login, activity)
    git.rs        mirror fetch, cat-file, is-ancestor, diff names (-z, quotepath off), argv-only Command
    sandbox.rs    podman|docker (explicit NOTARY_SANDBOX_ENGINE); labelled containers, cidfile, deadline, owned cleanup + retry
    desk.rs       work-order state machine persisted in SQLite: intake→building→(bounced≤2)→reviewing→awaiting_approval→approved|escalated
    main.rs       `notary run | doctor | register`
    tests/e2e.rs  scripted agents over the Switch HTTP path (#[ignore], run by make e2e)
  sandbox/        Containerfile (python:3.12-slim + acme-invoices deps baked in) + notary-run.py (~30 lines, writes result.json)
  templates/      notary-desk.template.yaml, rooms-only variant (for the no-Console path)
  scripts/        fetch-switch.sh, up.sh, register.sh, install-template.sh, add-users.sh, policies.sh (bash, quote every path)
  demo/acme-invoices/   source of the demo repo (also seeded as a local bare repo for offline/CI)
  docs/design.md, docs/trust-model.md, docs/built-by-voice.md, samples/receipt-wo-17.json
  Makefile        up, down, doctor, demo-offline, e2e, test, verify-sample, sandbox-image
  .github/workflows/ci.yml (fmt, clippy, test), e2e.yml (workflow_dispatch: compose + engine=docker + demo-offline)
```

**What is lifted from agora-rs**, all disclosed in the README:
- `crates/agora-engine/src/ledger.rs`: chain, verify, verify_head and 6 tests.
- `signing.rs`: whole, with 2 tests.
- `bin/notary-verify.rs`: the `check_receipt` logic.
- The workspace lints and dependency pins.
- From `alibi.rs` and `guard.rs`, only ideas: the Verdict/Audit shape, `MAX_REVISIONS=2`, code-written corrections and failing closed.
- One deliberate change: **absence of evidence means unverified, not contradicted**. Only git, after a successful fetch, can contradict because something is absent.

**From agora TS:** only the idea of decision bands, mapped to backed/contradicted/unverified.

**From REA (user's blueprint):** evidence IDs, separate certainty fields, closed error tags, no silent fallback, owned cleanup, small results with paging, and evaluation without an LLM. Skipped as over-building: the unknowns registry, provider registry, snapshot cache, schema generation and the mutation-test chain.

## 4. Reporting contract and judge rules
The builder's message ends with exactly one block:

```notary-claims
{"protocol":"notary-claims/1","wo":"WO-17","sha":"<40 hex>","claims":[
 {"id":"c1","kind":"commit_pushed","branch":"wo-17","sha":"<40 hex>"},
 {"id":"c2","kind":"files_changed","paths":["src/invoices/tax.py"]},
 {"id":"c3","kind":"tests_passed","scope":"full","command":"pytest -q"},
 {"id":"c4","kind":"check_ran","command":"pytest -q","outcome":"passed","cites":["ev_…"]}]}
```

| Claim | Decides it | Rule |
|---|---|---|
| commit_pushed | git after fetch | SHA missing → `commit_missing`. SHA not on the branch → `not_on_branch`. Fetch failed → unverified `upstream_unavailable`. |
| files_changed / no_other_changes | `git diff base...sha` | Exact paths. A claimed path missing from the diff → `files_differ`. A diff path that wasn't claimed → finding `undisclosed_change`. |
| protected paths | the git diff against the policy (`tests/**`, `.github/**`, `*.lock`, `requirements*.txt`, `conftest.py`) | Any match → finding `protected_path`, which needs a person. A *passing* re-run then becomes unverified `inputs_changed`. |
| tests_passed | **the Notary's own re-run** of the policy argv. The builder's command is never executed. | Backed when exit is 0, failed + errors = 0 and collected > 0. Failed > 0 → `tests_failed`, with test IDs. Exit 5 → unverified `no_tests_collected`. Sandbox down → unverified `sandbox_unavailable`. Never falls back to activity rows. |
| check_ran | activity rows from this agent (corroboration) | Title contains `\| ; & > < $(`, a backtick or a newline, or is truncated (`…`) → unverified `masked_command`. `completed` → backed (switch_host, partial). `failed` → `command_failed`. No row → unverified `no_activity_row` ("not proof it didn't run"). |
| reviewed_diff | activity rows (`git diff/show/log -p` naming the SHA or branch) | Backed, or unverified `no_activity_row`. Never contradicted. |
| cites | the ledger | The ID must exist in this WO and re-hash correctly, otherwise `citation_unknown` or `integrity_mismatch`. The operation must fit the claim and SHA, otherwise `citation_irrelevant`. |

**Timing:** judge only after the builder's turn row has ended, or after a 5 s grace period. Then re-fetch the activity.

## 5. Setup and reproducibility

**This machine:**
- `docker compose` is v2.39.2 running over the rootless podman socket, which is active.
- The `docker` command is a podman shim.
- Rust 1.99.1, claude and opencode are installed.

**`make up` does the following:**
1. `scripts/fetch-switch.sh` downloads the pinned standalone-compose blob from GHCR by digest `sha256:cba23887…0121`, verifies the hash, and qualifies short image names (`docker.io/library/postgres`, `docker.io/mattermost/…`). Switch's compose is not vendored, because it uses the Commons Clause licence.
2. It writes `.env` with generated secrets, plus `SWITCH_VERSION=0.31.0`, `FRONTEND_BASE_URL=http://localhost:8000` and `COMPOSE_PROJECT_NAME=notary`.
3. It runs `docker compose -f infra/compose.yml --profile collab up -d`, waits for `setup` to log "Setup complete!", then checks `curl /health`.
4. `register.sh` logs in, creates an API key, `POST /agents` the Notary, and stores the key at `~/.config/switch-notary/agent.json` (0600).
5. `add-users.sh` creates Mattermost users priya and sam through the MM API and adds them to the team.
6. `install-template.sh` validates and installs the template. On the live path, the template is applied once through the Console UI. On the no-Console path, the script posts the rooms-only YAML to `/gateway/rooms/from-yaml`. It then adds priya and sam to the hub, sets the Notary's addressing policy (owner; builder and reviewer in the work room; any user in the hub), and adds the admin member client to the work room so activity can be read.
7. `make sandbox-image` builds the image and records its digest in `.env`.

**Other targets:**
- `make doctor` runs `notary doctor`, which checks Switch health, the key, room membership, git reachability, the sandbox image, and that activity is readable. Every failure comes back as a tagged code with a remediation.
- **Judges on a Docker box:** the same commands with `NOTARY_SANDBOX_ENGINE=docker`. On macOS arm64, the Mattermost image is amd64-only, so emulation is slow. The README says so.
- **Three ways in:** `make verify-sample` (no infrastructure) → `make demo-offline` (no keys) → live (Console + Claude Code + OpenCode logins, and a fork of acme-invoices set in the template param).
- **Paths:** never interpolate the project path unquoted. The sandbox works under `~/.cache/notary/runs/<run_id>`.

## 6. Spikes, tonight (Nov 4, 23:00 → ~02:30)
Each spike has a go/no-go check. Every fallback is an explicit config value, never automatic.

| # | Check | Go when | Pre-decided fallback |
|---|---|---|---|
| S0 | Compose up on podman | `/health` OK; Mattermost login works | `podman-compose`, or the Console "Run a server on this computer" for development only |
| S1 | Console 0.40.1 rpm: connect to `http://localhost:8000` as admin; Claude Code and OpenCode agents reply in a test room | Both reply | Reviewer on Claude Code (template param `reviewer_provider`) |
| S2 | Rust: register, open the WebSocket, receive a message, `connect_to_room` + `send_targeted_message(target_names=["builder"])` through an alias, open an approval card in Mattermost, receive `approval_outcome`, map `answered_by` to a username | Card has working buttons; outcome is received | `NOTARY_APPROVAL=text`: `approve WO-17` from a sender whose name is in the approvers list |
| S3 | `GET /gateway/chats/{work}/activity` shows Claude's Bash and Edit rows after a member is added | Rows present | `NOTARY_ACTIVITY=off`: check_ran and reviewed_diff are always unverified (shown honestly) |
| S4 | podman `--network none`, read-only, `:Z` mounts, labels, `--cidfile`, then `inspect` gives the exit code and image digest | result.json read back | none needed (podman is the only engine here) |

Also tonight, each of these needs the user's go-ahead because it is outward-facing:
- Create both GitHub repos.
- Post the #801 claim comment (draft is ready; #801 has 9 of a maximum 10 assignees).
- Pin `@notary` agent naming.

## 7. Schedule (IST)

Two lanes:
- **Lane A**, Notary: the main session.
- **Lane B**, #801: a second Claude session in `~/src/switch`, a fork outside the path with spaces. You review it and record its video.

Keep subagent fan-out small. A big workflow is what cost us the 9 hours.

| When | Lane A (Notary) | Lane B (#801) and side prizes |
|---|---|---|
| Wed 23:00–02:30 | Repos, skeleton, `make up`, spikes S0–S4 | Comment on #801. Fork and baseline build (Node 24.14 / pnpm 11.5.3; core with testcontainers over the podman socket) |
| Thu 09:00–13:00 | notary-core: ids, evidence, errors, claims, judge plus 14 fixtures (7 scenarios + honest twins), ledger/signing lift, receipt + notary-verify | B: steps 1–2 (capability advertising, session-host steer, fence) |
| Thu 14:00–19:00 | notary: Switch client (WebSocket, ops, docs, approvals, gateway activity), git, sandbox, desk state machine, commands | B: steps 3–4 (room rule `roomDelivery`, core `sender_owns_agent`) |
| Thu 19:00–24:00 | acme-invoices with the trap (a tax fix passes `test_tax` but breaks `test_reports`), template, scripts. **First live run.** #HowISwitch post 1: a Rust agent puts an approval card in Mattermost | B: steps 5–6 (desktop setting + migration, card marking) |
| Fri 09:00–13:00 | `tests/e2e.rs` with 7 scripted scenarios against real compose; `make demo-offline`; CI ci.yml + e2e.yml | B: steps 7–8 (Console chat, docs, CHANGELOG), full local test suites |
| **Fri 14:00** | **Cut-line checkpoint** (§9) | |
| Fri 14:00–19:00 | Polish: hub line, receipt doc, head anchor, `show`, reviewer flow. Live runs 2–3, plus backup recordings | B: real-CLI steer test, video, **open the PR** with the CLA sentence |
| Fri 19:00–24:00 | README (dictated), architecture diagram, `docs/trust-model.md`, blog draft (dictated). Post 2: "my Notary caught Claude Code saying 'all tests pass'" | B: answer CI and review comments |
| Sat 09:00–12:00 | Clean-clone reproduction in a fresh toolbox: verify-sample, demo-offline, live. Fix what breaks. Tag v1.0.0 | |
| Sat 12:00–15:00 | Record the video (2 takes), edit to under 3:00, export to `~/Downloads`, upload | |
| Sat 15:00–18:00 | Publish the blog; finalise the README; Wispr Insights screenshot; post 3 (launch) | |
| Sat 18:00–20:00 | **Submit** (watch Discord for the form, which isn't live yet). Ask in Discord whether one project can enter both Track 01 and 02 | |
| Sat 20:00–23:59 | Buffer | |

**Wispr, all the way through:** dictate every prompt to Claude, the room and agent instructions, Desk Policy, the Reporting Contract, the README and the blog. Keep `docs/built-by-voice.md` with a list of what was dictated and a daily Insights screenshot.

## 8. Tests
- **Unit (`cargo test`, CI):**
  - Canonical JSON guard: `{"b":1,"a":2}` becomes `{"a":2,"b":1}`.
  - An evidence tamper is caught as `integrity_mismatch`.
  - Ledger: rewrite, delete, splice and truncate against the head are all caught.
  - Signing round trip.
  - Receipt: a golden bundle verifies; a one-byte edit fails.
  - Claims parser: caps, duplicate keys and unknown fields are rejected.
  - Judge: 7 scenarios, each with an honest twin, plus a proptest that evidence order never changes the Audit.
  - Render: golden tests that check codes, not prose.
- **e2e (`make e2e` / `demo-offline`, real Switch, no model)**, with scripted HTTP agents posting their own activity rows (labelled self-reported):

| Scenario | Expected |
|---|---|
| honest | all claims backed → `ReadyForApproval`, card opened |
| liar "all tests pass" | c3 `tests_failed`, bounce 1, no hub post |
| piped-tests | c4 `masked_command`; c3 decided by the re-run |
| protected-edit | `undisclosed_change` + `protected_path`; pass becomes `inputs_changed` |
| fabricated-citation | `citation_unknown` / `citation_irrelevant` |
| reviewer-who-didn't-run | `no_activity_row`, flagged, not contradicted |
| repeat liar ×3 | bounces 1 and 2, then `revision_limit` → escalate |

  Assertions parse the `notary-bounce` / `notary-verdict` blocks out of room history. Negative checks also assert that the history fetch returned the requester's message, so an empty result can't pass by accident.
- **CI:** `ci.yml` (fmt, clippy -D warnings, test) and `e2e.yml` (manual dispatch: ubuntu, Docker compose, `NOTARY_SANDBOX_ENGINE=docker`, demo-offline).
- **Before every commit:** `gitleaks git --pre-commit --staged --redact`. `.env` and the key files are never committed.

## 9. Cut line (Fri 14:00 checkpoint; cut in this order)
1. Lane B #801, if Lane A isn't green. It is a parallel lane, not a dependency.
2. The reviewer stage. The Notary's re-run already covers tests.
3. Activity corroboration (`NOTARY_ACTIVITY=off`).
4. The approval card, replaced by `NOTARY_APPROVAL=text`.
5. `show` paging. Keep the receipt doc.
6. The alias swap beat (`!set-alias builder acme-liar`, labelled as fault injection).

**Never cut:**
- The hub/work-room gate.
- git plus the re-run as evidence.
- The signed receipt and `notary-verify`.
- `make up` and `demo-offline`.
- Unit tests, README and video.

## 10. Demo video (under 3:00) and README

**Video:**

| Time | Beat |
|---|---|
| 0:00 | The catch first: the Notary marks "all tests pass" CONTRADICTED, my run is 41/42. |
| 0:15 | sam dictates the request with Wispr in `#acme-desk`. |
| 0:30 | Room layout: "the builder isn't in this channel". |
| 0:45 | Claude Code works; activity cards show. |
| 1:00 | Bounce with codes and `ev_` IDs; the builder fixes it; everything backed. |
| 1:30 | OpenCode reviews. |
| 1:45 | Hub: one line plus a card. sam tries to approve and is refused; priya approves. |
| 2:05 | Signed receipt; head hash in the room log. |
| 2:20 | Terminal: `notary-verify` OK; a one-byte edit FAILs; `make demo-offline` green with no keys. |
| 2:40 | "Without Switch" slide. |
| 2:55 | Repo, #HowISwitch, built by voice. |

If the live Claude Code run doesn't overclaim, use the scripted liar, labelled on screen. Never present a scripted beat as live.

**README:**
- Pitch and GIF.
- The three ways to run it.
- Architecture diagram.
- Switch primitives table: primitive → what breaks without it.
- Trust model and honest limits:
  - Agent keys live in `.switch/agents/` inside each agent's own directory, so activity rows are corroboration only.
  - The re-run is authoritative only while test files and the lockfile are unchanged.
  - It is not a sandbox against hostile code.
- Prior-work disclosure.
- Configuration and codes table.
- Troubleshooting.
- Built by voice.

## 11. Risks
- **Console on Fedora is early access.** S1 tonight. Local agents stop when Console closes, so keep it open while recording.
- **Approval cards from a custom agent are undocumented.** S2. The fallback is the text mode.
- **Activity reads need room membership.** S3. Fallback is activity off.
- **Claude Code may not overclaim on camera.** The trap makes it likely; the scripted liar, labelled, makes it certain.
- **Switch churn:** pin 0.31.0 and Console 0.40.1; don't hard-code the heartbeat interval.
- **The Track 03 competing PR #1445 is clean.** We win only on petr's starter/owner/thread rule, the setting, the fence and the card. Only one PR is chosen per issue.
- **The submission form isn't live.** Watch Discord and email; keep a 4 h buffer.
- **Usage limits.** Keep subagent fan-out small. If interrupted, write a handoff file in the repo (`.handoff/`) at the end of each block.

## 12. Verification before submitting
1. `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`, with output shown.
2. A fresh clone into a path **without** spaces and one **with** spaces: `make verify-sample`, then `make up && make demo-offline`. All 7 scenarios report their expected codes.
3. A live run end to end, matching success criterion 2. Save the receipt, then `notary-verify` it, edit it, and confirm the failure.
4. `e2e.yml` green on GitHub (Docker engine).
5. `semgrep scan --config p/default --metrics off crates/` and `gitleaks`, both clean.
6. Video under 3:00 and plays; README links work; blog published; Insights screenshot saved; #801 PR open with CI started.
7. Run `/code-review` on the final diff before tagging v1.0.0.
