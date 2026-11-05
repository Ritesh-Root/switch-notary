# Notary: a brief for building a front end

Everything here was checked against the running code on Nov 6 (commit `0d4bd3f`). The JSON
samples are real responses from a `make demo-offline` run.

## 1. What Notary is

Notary is a work-order desk for AI coding agents, built on [Switch](https://github.com/sandbox-quantum/switch).
A person files a request; an AI builder (Claude Code) fixes it and an AI reviewer (OpenCode)
reviews it. **Before anything the agents say reaches a person, the Notary checks every claim
against evidence it gathers itself.** Claims that don't hold go back to the builder with
the reason; a person only sees checked results, approves with one click, and gets a signed
receipt anyone can verify offline.

The one-line pitch: *AI coworkers that have to show receipts.*

## 2. How it works

### The people and agents
| Who | Where | Role |
|---|---|---|
| **sam** | desk room | requester: files work orders |
| **priya** | desk room | approver (named in the Desk Policy): approves or holds |
| **builder** (`acme-builder`, Claude Code) | work room only | fixes the code, pushes a branch, reports with claims |
| **reviewer** (`acme-reviewer`, OpenCode) | work room only | reviews the diff, reports with claims |
| **notary** (`@notary`, our Rust agent) | both rooms | checks every claim, bounces bad reports, opens the approval card, signs the receipt |

There are two rooms (Mattermost channels via Switch):
- **Desk** (`acme-desk`): people and the Notary. **The builder and reviewer are not members**, so
  unchecked words can't reach people. The gate is the room layout itself.
- **Work room** (`acme-work`): builder, reviewer and Notary. All the back-and-forth happens here.

### One work order, start to finish
1. **Intake.** sam writes in the desk: `@notary new Globex invoices over $10k show the wrong tax`.
   The Notary opens **WO-N**, records the repo's `main` commit as the base, loads the Desk Policy
   (approvers, the test command, protected paths, max bounces) and tells the builder in the work room.
2. **Build.** The builder pushes branch `wo-N` and reports. The report ends with a
   `notary-claims` JSON block listing claims such as "I pushed commit X", "I changed these
   files", "all tests pass", "I ran `pytest -q`".
3. **Check.** For each claim the Notary gathers its own evidence:
   - **git**: does the commit exist, is it on the branch, which files really changed.
   - **its own test run**: it re-runs the policy's test command in a locked-down container
     (no network, read-only), at that commit. It never runs the builder's command.
   - **Switch's activity log**: which commands the agent ran (corroboration only).
   Every piece of evidence is sealed with a content hash (`ev_…`) into a hash-chained ledger.
4. **Judge.** Each claim gets one of three verdicts:
   - **backed** (green): the evidence supports it.
   - **contradicted** (red): the evidence says otherwise, e.g. `tests_failed`: "my run is 17/18".
   - **unverified** (amber): no evidence either way. *Absence of evidence is never treated as a lie.*
5. **Bounce or pass.** Anything contradicted goes back to the builder as a `notary-bounce`
   block with codes, a fix instruction and evidence IDs. After 2 bounces it stops and a
   person decides (`revision_limit`). A clean report goes to the reviewer, whose claims are judged
   the same way.
6. **Approve.** The desk gets one line ("WO-6 ready for approval: 4/4 claims backed…") and an
   approval card. Only approvers count: if sam presses approve, the Notary refuses and opens a
   new card.
7. **Receipt.** On approval the Notary signs the ledger head (Ed25519), posts the head hash in the
   desk as a witness and uploads `receipt-wo-N.json`. Anyone can run `notary-verify` on it; change
   one byte and it fails.

### Work-order states
`building` → `reviewing` → `awaiting_approval` → `approved` | `held`

- Bounces keep the order in `building` (or `reviewing`).
- Running out of bounces sends it to `awaiting_approval` with the flag `revision_limit`; a person decides.
- (There is no separate `escalated` state, although the current viewer has a colour for one.)

## 3. The data a front end can use today

The Notary already runs a small read-only HTTP server: **`http://127.0.0.1:8090`**, with no
login, localhost only, and nothing in it changes state. Everything below comes from it.

### `GET /api/state`: one call for the whole screen
```json
{
  "key": "431d229c445b0fd6…",
  "desk": [ /* message, oldest first, up to 120 */ ],
  "work": [ /* message, oldest first, up to 120 */ ],
  "work_orders": [
    {"wo": "WO-6", "title": "Lazy reviewer: says it reviewed, ran nothing", "requester": "sam", "state": "held", "attempts": 1},
    {"wo": "WO-5", "title": "Fabricated: cites made-up evidence", "requester": "sam", "state": "building", "attempts": 1}
  ],
  "receipts": [
    {"file": "receipt-wo-1.json", "wo": "WO-1", "ok": true, "entries": 13, "head": "62eb84daae712472b5f7…"}
  ]
}
```
- `key`: the Notary's public signing key (hex).
- `work_orders`: newest first, at most 50. `attempts` is the number of builder reports so far.
- `receipts`: each receipt is checked server-side with the same code as `notary-verify`. A broken one is
  `{"file": "...", "ok": false, "why": "record dc_… was changed: it no longer hashes to its id"}`.
- **A message** is
  `{"sender_name": "notary", "sender": "@switch-agent-…:localhost", "body": "…", "timestamp": 1793915461329}`
  where `timestamp` is in ms. If a room can't be read, the list holds one message from `sender_name: "viewer"`
  that explains why.

### `GET /api/ledger/{WO-N}`: the evidence trail of one work order
```json
{"wo": "WO-1", "chain": "13 entries, chain intact", "lines": [
  "1. `po_d6e111c7b8a` intake + Desk Policy",
  "2. `rp_b17fb4f7444` claims from demo-builder: 4 claim(s)",
  "3. `ev_4c0cffe0591` git_commit by git: {\"exists\":true,\"on_branch\":true}",
  "4. `ev_939eb1fbc4c` git_diff by git: {\"paths\":[\"src/invoices/reports.py\",…]}",
  "5. `ev_704334d7fca` notary_test_run by notary_run: {\"collected\":18,\"failed\":0,…}",
  "6. `ev_9ac1b40510d` switch_activity by switch_host: {\"rows\":[{\"status\":\"completed\",\"title\":\"pytest -q\"}]}",
  "7. `au_af4b2f1746a` audit of demo-builder report 1: c1:backed c2:backed c3:backed c4:backed",
  "…", "13. `dc_…` decision: approve by priya"]}
```
- `chain` is either "N entries, chain intact" or "broken at entry N: reason".
- Lines are pre-formatted text. The ID prefix gives the kind: `po_` policy, `rp_` report, `ev_` evidence,
  `au_` audit, `dc_` decision.

### `GET /receipts/receipt-wo-N.json`: download the signed receipt
The response is the full bundle, served as an attachment:
```json
{"protocol": "notary-receipt/1",
 "head": {"run": "WO-1", "entries": 13, "head": "62eb…", "at_ms": 1793830324823, "key": "431d…", "signature": "49df…"},
 "entries": [{"seq": 1, "at_ms": …, "kind": "policy", "record": "po_d6e1…", "prev": "0000…", "hash": "b4d9…"}, …],
 "records": {"au_18ce…": {"protocol": "notary-audit/1", "wo": "WO-1", "stage": "reviewer", "agent": "demo-reviewer",
             "checked": [{"claim": "c1", "kind": "reviewed_diff", "verdict": "backed", "evidence": ["ev_37f1…"]},
                         {"claim": "c2", "kind": "check_ran", "verdict": "backed", "evidence": ["ev_37f1…"],
                          "limitations": ["switch_host: reported with the agent's own key; corroboration, not proof"]}],
             "findings": [], …}, …}}
```
The receipt is the richest structured source: every audit with per-claim verdicts and evidence.

### The structured blocks inside messages
Messages are Markdown-ish text: `` `code` ``, `**bold**`, and fenced blocks. Three block
types carry structured data. Parse them with ```` /```([\w-]*)\n([\s\S]*?)```/g ````:

**`notary-claims`**: what an agent claims (from the builder or reviewer)
```json
{"protocol": "notary-claims/1", "wo": "WO-1", "sha": "7180cf45…", "claims": [
  {"id": "c1", "kind": "commit_pushed", "branch": "wo-1", "sha": "7180cf45…"},
  {"id": "c2", "kind": "files_changed", "paths": ["src/invoices/tax.py", "src/invoices/reports.py"]},
  {"id": "c3", "kind": "tests_passed", "scope": "full", "command": "pytest -q"},
  {"id": "c4", "kind": "check_ran", "command": "pytest -q", "outcome": "passed"}]}
```
Claim kinds: `commit_pushed`, `files_changed`, `no_other_changes`, `tests_passed`, `check_ran`, `reviewed_diff`.

**`notary-bounce`**: sent back to the builder (work room)
```json
{"protocol": "notary-bounce/1", "wo": "WO-2", "sha": "95b41c7d…", "attempt": 1, "max": 2, "audit": "au_af99…",
 "backed": ["c1", "c2", "c4"],
 "fix": [{"claim": "c3", "code": "tests_failed", "fixer": "builder", "retryable": true,
          "do": "Fix the listed tests; the Notary's own run at the sha decides.",
          "facts": {"collected": 18, "passed": 17, "failed": 1, "errors": 0,
                    "ids": ["tests/test_reports.py::test_report_reconciles_with_invoices"]},
          "ev": ["ev_836fd809…"]}],
 "unverified": []}
```
`fix` = red (the builder must fix it), `unverified` = amber, `backed` = green. `facts` varies by code
(failing test IDs, paths, counts).

**`notary-verdict`**: the verdict on a report that wasn't bounced (work room)
```json
{"protocol": "notary-verdict/1", "wo": "WO-1", "sha": "7180cf45…", "agent": "demo-builder", "stage": "builder",
 "attempt": 1, "audit": "au_af4b…", "next": "ready",
 "checked": [{"claim": "c1", "kind": "commit_pushed", "code": null}, …],
 "findings": [{"code": "undisclosed_change", "claim": null, "paths": ["tests/test_reports.py"]}]}
```
`code: null` means backed. `next` is `ready` | `bounce` | `escalate`. **Note:** this block gives the
code but not whether it was contradicted or unverified, so a front end has to map codes (table below).

Plain-text desk lines worth recognising: `**WO-N ready for approval**: …`, `**WO-N approved by priya.** Signed ledger head …`,
`WO-N held by priya.`, and the refusal `… isn't an approver for WO-N`.

## 4. The verdict codes (closed set of 23)

| Code | Colour | Who fixes | Meaning |
|---|---|---|---|
| `block_missing` | red | builder | report has no `notary-claims` block |
| `block_invalid` | red | builder | block isn't strict JSON / breaks limits |
| `commit_missing` | red | builder | the commit isn't in the repo after fetch |
| `not_on_branch` | red | builder | the commit isn't on the claimed branch |
| `files_differ` | red | builder | claimed files ≠ the real git diff |
| `undisclosed_change` | red | builder | changed a file without saying so |
| `tests_failed` | red | builder | the Notary's own test run failed (names the tests) |
| `command_failed` | red | builder | Switch recorded the command as failed |
| `citation_unknown` | red | builder | cited an `ev_` ID the Notary never published |
| `citation_irrelevant` | red | builder | cited evidence about something else |
| `masked_command` | amber | builder | check ran through a pipe/`;`/etc., so it can't be trusted |
| `no_tests_collected` | amber | builder | 0 tests ran, so an empty run isn't a pass |
| `no_activity_row` | amber | person | Switch has no record of the command (not proof it didn't run) |
| `inputs_changed` | amber | person | tests or lockfile changed, so a pass can't back the claim |
| `protected_path` | amber | person | touched a protected path (`tests/**`, `.github/**`, lockfiles); an approver decides |
| `revision_limit` | amber | person | bounces used up; a person decides |
| `not_an_approver` | red | person | someone not on the approver list pressed approve |
| `upstream_unavailable` | amber | Notary | git or Switch didn't answer |
| `sandbox_unavailable` | amber | operator | the test container image isn't loaded |
| `sandbox_failed` | amber | Notary | the test run timed out or failed its handshake |
| `cleanup_incomplete` | amber | Notary | a test container couldn't be removed |
| `integrity_mismatch` | red | person | a ledger record changed after it was written |
| `configuration_invalid` | red | operator | a setting is wrong (there are no silent defaults) |

## 5. Constraints a front end must respect

- **Read-only by design.** People act in chat (Mattermost via Switch): filing, approving and holding all
  happen there, which keeps Switch at the centre of the product. A front end that *acts* would need new
  endpoints.
- **Same origin.** The server sends no CORS headers. Either serve the front end from the Notary (the
  current page is compiled in from `crates/notary/src/viewer.html`), or proxy `/api` and `/receipts` from
  your dev server (e.g. Vite's `server.proxy` → `http://127.0.0.1:8090`).
- **Polling.** There's no push stream. The current page polls `/api/state` every 2.5 s and re-renders
  only when the response text changes.
- **Untrusted text.** Message bodies come from AI agents: escape everything, and never inject a body as HTML.
- **Sizes.** At most 120 messages per room and 50 work orders.

## 6. What the current viewer shows (the baseline to beat)

There are three columns: the **Desk** feed, the **Work room** feed, and a side panel with **Work orders** (state
badges) and **Receipts** (OK/FAIL, download). Blocks render as coloured chips per claim. Clicking a work
order or receipt opens its ledger in a dialog. Under 1100 px it switches to tabs. It uses a warm light and dark
theme, Inter for text and the system mono font for code.

## 7. Gaps worth closing for a richer front end (small backend additions)

All of these are cheap to add on the Rust side; ask and they can be built:
- `GET /api/wo/{WO-N}`: one work order with its audits as structured JSON (per-claim verdict
  backed/contradicted/unverified, codes, facts, evidence), so the UI doesn't parse message text.
- `GET /api/evidence/{ev_…}`: one evidence record in full (authority, confidence, coverage, the raw result).
- A `verdict` field on each `checked` item in `notary-verdict` blocks, so no code→colour table is needed.
- Server-sent events instead of polling.
