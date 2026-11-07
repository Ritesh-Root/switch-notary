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
- There is no separate `escalated` state.

## 3. The data a front end can use today

The Notary already runs a small read-only HTTP server: **`http://127.0.0.1:8090`**, with no
login, localhost only, and nothing in it changes state. Everything below comes from it.

### `GET /`: the viewer page

Read off the `/` route in `crates/notary/src/viewer.rs` on Nov 7, from the source. It is not a JSON
route, and it was not in the Nov 6 `make demo-offline` capture, so there is no sample body from that
run here. The handler is `Html(PAGE)`, and `PAGE` is `include_str!("viewer.html")`: one HTML document
compiled into the binary. The router has no other HTML route and no asset route. Section 6 describes
what that page draws.

A local `Host` is `127.0.0.1`, `localhost`, or `::1`, with or without a port (`local_host` in
`viewer.rs`). Anything else is **421**, same as every other route. `get_root_serves_the_viewer_html`
requests `http://127.0.0.1:{port}/` and checks the 200 row. `foreign_host_is_misdirected_on_every_other_route`
sends `Host: evil.example` to `/` and checks the 421 row.

| Status | When | Body |
|---|---|---|
| 200 | Host is local | the viewer HTML. `Content-Type: text/html; charset=utf-8`. The body starts with `<!doctype html>` and contains `<title>Notary</title>` and `id="case"`. |
| 421 | any other Host | `open the viewer at http://127.0.0.1 or http://localhost` |

The handler takes no state. It does not open `desk.db`, the ledger, or `receipts/`, so a missing file
does not change this response. The page then polls the JSON routes below.

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

### `GET /api/wo/{WO-N}`: one work order's whole case file (the page uses this)
```json
{"wo": "WO-2",
 "order": {"wo": "WO-2", "title": "Liar: …", "requester": "sam", "state": "awaiting_approval", "attempts": 3,
           "base": "de3922b4…", "branch": "wo-2", "created_ms": 1793830326796},
 "chain": {"ok": true, "entries": 19},
 "entries": [{"seq": 1, "at_ms": …, "kind": "policy", "record": "po_fa7d…", "prev": "0000…", "hash": "…"}, …],
 "records": {"po_fa7d…": {"protocol": "notary-intake/1", "policy": {"approvers": ["priya"], "acceptance": ["pytest", "-q"], "max_bounces": 2, …}, …},
             "rp_30a3…": {"agent": "demo-builder", "report": {"claims": [{"id": "c3", "kind": "tests_passed", "command": "pytest -q"}, …]}},
             "ev_836f…": {"authority": "notary_run", "operation": "notary_test_run", "result": {"collected": 18, "passed": 17, "failed": 1, "failed_ids": […]}, …},
             "au_af99…": {"stage": "builder", "attempt": 1, "report": "rp_30a3…",
                          "checked": [{"claim": "c3", "kind": "tests_passed", "verdict": "contradicted", "code": "tests_failed", "evidence": ["ev_836f…"], "facts": {…}}, …],
                          "findings": []},
             "dc_…": {"by": "sam", "answer": "approve", "counted": false}},
 "codes": {"tests_failed": {"fixer": "builder", "retryable": true, "remediation": "Fix the listed tests; …"}, …}}
```
- The same entries and records a receipt holds, read live from the ledger, so it works before approval too.
- Every `checked` item carries `verdict` (`backed` | `contradicted` | `unverified`); no code table needed.
- `codes` explains every code in the audits, plus `revision_limit` and `not_an_approver`.
- `chain` is `{"ok": false, "error": "broken at entry N: …"}` if the ledger was tampered with.
- 400 for a malformed ID, 404 for an unknown work order.

### Types for `GET /api/state` and `GET /api/wo/{WO-N}`

Read off `state` and `case_file` in `crates/notary/src/viewer.rs` on Nov 7, from the source. The JSON samples above remain the Nov 6 `make demo-offline` responses.

A local `Host` (`127.0.0.1`, `localhost`, or `::1`) is required on every route. Anything else is **421** with the plain text `open the viewer at http://127.0.0.1 or http://localhost`. The other non-200 bodies are plain text too, and only `/api/wo/{id}` returns them:

| Status | When | Body |
|---|---|---|
| 400 | id is not `WO-` plus 1–9 digits (any case; the handler uppercases it) | `not a work order` |
| 404 | `desk.db` has no row for that id | `no such work order` |
| 503 | `desk.db` can't be opened, or the row query fails | `couldn't read the desk: ` plus the SQLite error |
| 503 | `ledger.db` can't be opened | the ledger error, `{code}: {detail}` |

`GET /api/state` is 200 when the host is local, including when a room, `desk.db`, or `receipts/` can't be read.

```ts
/** 200 JSON from GET /api/state. */
interface DeskState {
  /** Hex public signing key, full string, as loaded. */
  key: string;
  /** See RoomFeed. */
  desk: RoomFeed;
  work: RoomFeed;
  /** Newest first, at most 50. [] when desk.db can't be read. */
  work_orders: WorkOrderRow[];
  /** Checked with the same code as notary-verify. [] when receipts/ can't be read. */
  receipts: ReceiptCheck[];
}

/**
 * On success, Switch's `events` value copied through (an array of RoomMessage
 * when history matches the shape Agent::history documents). If that key is
 * missing, JSON null. If the room can't be read, one RoomReadError.
 */
type RoomFeed = RoomMessage[] | [RoomReadError] | null;

/** Fields the page reads. Switch may add others; this server does not strip them. */
interface RoomMessage {
  sender_name?: string;
  sender?: string;
  body?: string;
  /** Milliseconds. */
  timestamp?: number | null;
}

interface RoomReadError {
  sender_name: "viewer";
  body: string;
  timestamp: null;
}

interface WorkOrderRow {
  wo: string;
  title: string;
  requester: string;
  /** Text from the desk. The states the desk writes are in section 2. */
  state: string;
  /** Builder reports so far (SQLite integer). */
  attempts: number;
}

interface ReceiptOk {
  file: string;
  /** head.run from the receipt, the work-order id. */
  wo: string;
  ok: true;
  /** How many entries, not the entries. */
  entries: number;
  head: string;
}

interface ReceiptBad {
  file: string;
  ok: false;
  why: string;
}

type ReceiptCheck = ReceiptOk | ReceiptBad;

/** 200 JSON from GET /api/wo/{WO-N}. */
interface CaseFile {
  wo: string;
  order: WorkOrderDetail;
  chain: ChainOk | ChainBroken;
  entries: LedgerEntry[];
  /** Record id to the stored JSON value. {} when the export fails; entries may still be present. */
  records: Record<string, unknown>;
  /** Always includes revision_limit and not_an_approver, plus every known code under checked and findings of au_* records. Unknown code strings are skipped. Keys are sorted. */
  codes: Record<string, CodeMeaning>;
}

interface WorkOrderDetail extends WorkOrderRow {
  base: string;
  branch: string;
  created_ms: number;
}

interface ChainOk {
  ok: true;
  entries: number;
}

interface ChainBroken {
  ok: false;
  error: string;
}

interface LedgerEntry {
  seq: number;
  at_ms: number;
  kind: "evidence" | "policy" | "report" | "audit" | "decision";
  record: string;
  prev: string;
  hash: string;
}

interface CodeMeaning {
  retryable: boolean;
  fixer: "builder" | "human" | "operator" | "notary";
  remediation: string;
}
```

### `GET /api/ledger/{WO-N}`: the evidence trail of one work order, as text
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

### Types for `GET /api/ledger/{WO-N}` and `GET /receipts/{file}`

Read off `ledger` and `receipt_file` in `crates/notary/src/viewer.rs`, `Bundle` in
`crates/notary-core/src/receipt.rs`, and `SignedHead` in `crates/notary-core/src/signing.rs`
on Nov 7, from the source. The status rows are what
`get_api_ledger_returns_the_chain_or_a_plain_error` and
`get_receipt_file_downloads_the_bundle_or_404` assert. The 421 host rule above applies
to both routes. The JSON samples above remain the Nov 6 `make demo-offline` responses.

| Status | Route | When | Body |
|---|---|---|---|
| 200 | `GET /api/ledger/{id}` | id is `WO-` plus 1–9 digits, and `ledger.db` opens | `LedgerTrail` JSON. An id with no rows is still 200: `chain` is `0 entries, chain intact` and `lines` is `[]`. |
| 400 | `GET /api/ledger/{id}` | id fails that check | `not a work order` |
| 503 | `GET /api/ledger/{id}` | `Ledger::open` fails | `{code}: {detail}`. The missing-directory case the test hits starts with `upstream_unavailable: sqlite:`. |
| 200 | `GET /receipts/{file}` | name is `receipt-wo-` plus one or more digits plus `.json`, and that file reads | the stored bytes, unchanged. `Content-Type: application/json`. `Content-Disposition: attachment; filename="{file}"`. A file the Notary wrote is a `ReceiptBundle`. The handler returns the bytes whether or not they parse. |
| 404 | `GET /receipts/{file}` | the name fails that check, or the file is missing or unreadable | empty |

```ts
/** 200 JSON from GET /api/ledger/{WO-N}. `chain` is a string here. */
interface LedgerTrail {
  /** Uppercased by the handler. */
  wo: string;
  /**
   * "{n} entries, chain intact" when Ledger::verify returns n (n can be 0).
   * "broken at entry {seq}: {reason}" when it returns Broken. A failed entry
   * read uses seq 0 and still responds 200. See ledger() in viewer.rs.
   */
  chain: string;
  /**
   * One preformatted line per entry, in seq order, from ledger_lines.
   * [] when that work order has no entries. Also [] when the entry read
   * fails; `chain` then starts with "broken at entry 0:".
   */
  lines: string[];
}

/**
 * The file the Notary writes: Bundle in receipt.rs, protocol notary-receipt/1.
 * GET /receipts/{file} serves those bytes with the headers above. It does not
 * parse them, so a 200 body is this shape only when the file on disk is.
 */
interface ReceiptBundle {
  /** receipt::verify rejects any other string. */
  protocol: "notary-receipt/1";
  head: SignedHead;
  /** Same object as LedgerEntry above. */
  entries: LedgerEntry[];
  /** Record id to the stored JSON value. BTreeMap, so the keys are sorted. */
  records: Record<string, unknown>;
}

/** SignedHead in crates/notary-core/src/signing.rs. Numbers are JSON numbers. */
interface SignedHead {
  /** Work-order id (head.run). The receipt list calls this `wo`. */
  run: string;
  /** How many entries the signature covers. */
  entries: number;
  /** Hash of the last entry. */
  head: string;
  at_ms: number;
  /** Hex Ed25519 public key. */
  key: string;
  /** Hex Ed25519 signature over SignedHead::statement. */
  signature: string;
}
```

### `GET /api/evidence/{ev_…}`: one evidence record by id

Read off `evidence` in `crates/notary/src/viewer.rs` on Nov 7, from the source. This route was not in the Nov 6 `make demo-offline` capture, so there is no sample body from that run here. The same object is the value at `records[id]` in `GET /api/wo/{WO-N}` when that work order's ledger names the id. The 421 host rule above applies. Statuses below are what `get_api_evidence_returns_the_record_or_a_plain_error` asserts.

| Status | When | Body |
|---|---|---|
| 200 | id is `ev_` plus 64 lowercase hex digits, `ledger.db` opens, and the stored bytes still hash to that id | `EvidenceRecord` JSON |
| 400 | id fails that check (any other prefix, a short id, or uppercase hex) | `not evidence` |
| 404 | the id is well formed and the ledger opens, but `records` has no such row | `no such evidence` |
| 409 | the row exists and its bytes no longer hash to the id | `integrity_mismatch: record {id} changed after it was written` |
| 503 | `Ledger::open` fails | `{code}: {detail}`. The missing-directory case the test hits starts with `upstream_unavailable: sqlite:`. |

```ts
/** 200 JSON from GET /api/evidence/{ev_…}. */
interface EvidenceRecord {
  /** The path id, unchanged. */
  id: string;
  /**
   * Stored JSON, re-hashed on the way out (`Ledger::record`). The handler
   * does not parse it. A record the Notary sealed is an evidence body
   * (authority, confidence, coverage, subject, operation, params, result,
   * limitations); see `Body` in `crates/notary-core/src/evidence.rs`.
   */
  record: unknown;
}
```

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
- **Polling.** There's no push stream. The current page polls `/api/state` (and the open work order's
  `/api/wo/{WO-N}`) every 2.5 s and re-renders only when a response changes.
- **Untrusted text.** Message bodies come from AI agents: escape everything, and never inject a body as HTML.
- **Sizes.** At most 120 messages per room and 50 work orders.

## 6. What the current front end shows

The page at `/` (`crates/notary/src/viewer.html`, compiled into the binary; no build step) has two views:

- **Work orders** (default). A rail lists every work order with its state and a progress bar. Selecting one
  (or opening `/#WO-2`) shows its case file:
  - a five-step track: Filed, Build (reports, bounces), Review, Approval, Receipt;
  - one status line saying what it's waiting for and what's flagged;
  - **Claims checked**: one tab per report; each claim shows what the agent said, the verdict, what Notary
    found itself (git, its own test run, Switch's activity log) and what the code means;
  - **Decisions**: every approve/hold press, including refused ones;
  - **Receipt**: verified status, download, and the `notary-verify` output;
  - **Evidence trail**: every ledger entry in plain words; open one for its raw record;
  - **Conversation**: the messages from both rooms that mention this work order.
- **Live rooms** (`/#rooms`): the Desk and Work room feeds side by side.

Under 1100 px it becomes list → detail with a back link; the claims table stacks on phones.
Chips show state with a symbol as well as colour: backed ✓ green, contradicted ✕ red, unverified ? gold dashed.

Palette: Pure White `#F5F3EF` (background), Midnight Black `#08090B` (text, people's avatars), Vibrant Golden
Yellow `#FFC900` (the Notary's own messages, anything waiting on a person); gold used as text is darkened to
`#7D6000` so it stays readable on white. Source Serif 4 for the wordmark and
numbers, Inter for text, JetBrains Mono for IDs and code (Google Fonts, with system fallbacks).

## 7. Still open

- A `verdict` field on each `checked` item in `notary-verdict` chat blocks (the ledger audits already have it).
- Server-sent events instead of polling.
