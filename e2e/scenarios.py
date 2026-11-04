"""Keyless end-to-end scenarios for Notary, against a real Switch + Mattermost.

The builder and reviewer are scripted HTTP agents (no model): each scenario plants a known
behaviour (an honest fix, a lie about the tests, a piped command, an edit to a protected test,
a made-up citation, a reviewer who ran nothing) and asserts the Notary's verdict codes and
evidence ids, never its prose. Humans (sam, priya) act through Mattermost's API.

Needs: make up, make install-offline, the Notary running (make run). Standard library only.
Run: python3 -I e2e/scenarios.py [scenario ...]
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STATE = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "switch-notary"


def load_env(path):
    env = {}
    for line in Path(path).read_text().splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip()
    return env


ENV = {**load_env(ROOT / "infra/.env"), **load_env(ROOT / "infra/notary.env")}
SWITCH = os.environ.get("NOTARY_SWITCH_URL", "http://127.0.0.1:8000")
MM = os.environ.get("NOTARY_MATTERMOST_URL", "http://127.0.0.1:8065")
HUB, WORK = ENV["NOTARY_HUB_ROOM"], ENV["NOTARY_WORK_ROOM"]
SCRIPTED = json.loads((STATE / "scripted.json").read_text())
BUILDER, REVIEWER = ENV["NOTARY_BUILDER"], ENV["NOTARY_REVIEWER"]
REPO_URL = ENV["NOTARY_REPO_URL"]
DATA = Path(ENV["NOTARY_DATA_DIR"])
TEAM = ENV.get("MATTERMOST_TEAM_NAME", "switch")
DESK_CHANNEL = BUILDER.split("-")[0] + "-desk"


def http(method, url, body=None, headers=None, raw=False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            text = r.read()
            if raw:
                return r, text
            return json.loads(text) if text else None
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"{method} {url} -> {e.code}: {e.read()[:300]!r}") from None


# ---------- Switch, as a scripted agent ----------


class Scripted:
    def __init__(self, name):
        self.name = name
        self.id = SCRIPTED[name]["id"]
        self.key = SCRIPTED[name]["api_key"]
        self.auth = {"Authorization": f"Bearer {self.key}"}

    def post(self, room, content):
        return http("POST", f"{SWITCH}/agents/{self.id}/message", {"room_id": room, "content": content}, self.auth)

    def history(self, room, limit=80):
        return http("GET", f"{SWITCH}/agents/{self.id}/rooms/{room}/history?limit={limit}", None, self.auth)["events"]

    def activity(self, title, status, room=WORK, n=[0]):
        """Report a tool call the way Switch Console's session host would (self-reported: no Console here)."""
        n[0] += 1
        now = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
        turn = f"t{time.time_ns()}-{n[0]}"  # unique across runs: Switch keeps only the newest revision of an item
        body = {
            "turn_id": turn, "item_id": json.dumps([turn, "i1"]), "kind": "tool-activity", "revision": 1,
            "status": status, "title": title, "text": "", "command_id": None, "room_id": room, "thread_id": None,
            "message_id": None, "occurred_at": now,
        }
        http("POST", f"{SWITCH}/agent-sessions/scripted-{self.name}/activity", body, self.auth)


# ---------- Mattermost, as a person ----------


def mm_login(user):
    password = ENV[f"DEMO_PASSWORD_{user.upper()}"] if user in ("priya", "sam") else ENV["MATTERMOST_USER_PASSWORD"]
    req = urllib.request.Request(f"{MM}/api/v4/users/login", data=json.dumps({"login_id": user, "password": password}).encode(),
                                 headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=30) as r:
        return {"Authorization": f"Bearer {r.headers['Token']}"}


def mm_channel(auth, name):
    team = http("GET", f"{MM}/api/v4/teams/name/{TEAM}", None, auth)["id"]
    return http("GET", f"{MM}/api/v4/teams/{team}/channels/name/{name}", None, auth)["id"]


def mm_say(user, channel, text):
    auth = mm_login(user)
    http("POST", f"{MM}/api/v4/posts", {"channel_id": mm_channel(auth, channel), "message": text}, auth)


def mm_card(user, channel, wo, after, exclude=()):
    """The newest approval card for `wo` posted after `after` (epoch ms) and not in `exclude`: (post id, auth, actions)."""
    auth = mm_login(user)
    cid = mm_channel(auth, channel)
    posts = http("GET", f"{MM}/api/v4/channels/{cid}/posts?per_page=40", None, auth)
    for pid in posts["order"]:
        p = posts["posts"][pid]
        actions = [a for at in (p.get("props") or {}).get("attachments") or [] for a in at.get("actions") or []]
        if actions and pid not in exclude and wo in p["message"] + json.dumps(p.get("props")) and p["create_at"] > after:
            return pid, auth, actions
    return None, auth, []


def press(user, channel, wo, label, after, exclude=()):
    """Press `label` on the newest card for `wo`; returns the card's post id."""
    pid, auth, actions = wait(lambda: (lambda c: c if c[0] else None)(mm_card(user, channel, wo, after, exclude)), f"approval card for {wo}")
    action = next(a for a in actions if label.lower() in a["name"].lower())
    http("POST", f"{MM}/api/v4/posts/{pid}/actions/{action['id']}", {}, auth)
    return pid


# ---------- git ----------


def git(*args, cwd):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})


def git_out(*args, cwd):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


FIN12 = '''    subtotal = sum(lines, Decimal("0.00"))
    if subtotal >= HIGH_VALUE:
        return round_cents(subtotal * rate)
    return sum((line_tax(amount, rate) for amount in lines), Decimal("0.00"))'''
OLD_TAX = '    return sum((line_tax(amount, rate) for amount in lines), Decimal("0.00"))'
NEW_TEST = '''from decimal import Decimal

from invoices.tax import invoice_tax


def test_high_value_invoice_is_taxed_on_the_subtotal():
    # FIN-12: 4 x 2,500.03 = 10,000.12 -> 18% of the subtotal, rounded once: 1,800.02
    assert invoice_tax([Decimal("2500.03")] * 4) == Decimal("1800.02")


def test_exactly_ten_thousand_counts_as_high_value():
    assert invoice_tax([Decimal("5000.00"), Decimal("5000.00")]) == Decimal("1800.00")
'''


def make_change(clone, kind):
    """Edit the clone; return the paths changed."""
    tax = clone / "src/invoices/tax.py"
    tax.write_text(tax.read_text().replace(OLD_TAX, FIN12))
    (clone / "tests/test_fin12.py").write_text(NEW_TEST)
    paths = ["src/invoices/tax.py", "tests/test_fin12.py"]
    if kind in ("honest", "protected"):
        rep = clone / "src/invoices/reports.py"
        text = rep.read_text().replace("from .tax import line_tax", "from .tax import invoice_tax").replace(
            '        charged = sum((line_tax(amount) for amount in inv.lines), Decimal("0.00"))', "        charged = invoice_tax(list(inv.lines))")
        rep.write_text(text)
        paths.append("src/invoices/reports.py")
    if kind == "protected":
        t = clone / "tests/test_reports.py"
        t.write_text(t.read_text() + "\n# tidied by the builder\n")
    return paths


def push_fix(wo, branch, kind):
    with tempfile.TemporaryDirectory() as tmp:
        clone = Path(tmp) / "repo"
        git("clone", "-q", REPO_URL, str(clone), cwd=tmp)
        git("switch", "-q", "-c", branch, cwd=clone)
        paths = make_change(clone, kind)
        git("add", "-A", cwd=clone)
        git("-c", "user.name=demo-builder", "-c", "user.email=builder@acme.test", "commit", "-q", "-m", f"{wo}: tax invoices of 10,000 or more on the subtotal (FIN-12)", cwd=clone)
        git("push", "-q", "-f", "origin", branch, cwd=clone)
        return git_out("rev-parse", "HEAD", cwd=clone), paths


# ---------- waiting for the Notary ----------


def wait(fn, what, timeout=180):
    end = time.time() + timeout
    while time.time() < end:
        got = fn()
        if got:
            return got
        time.sleep(1.5)
    raise AssertionError(f"timed out waiting for {what}")


def blocks(body, tag):
    out = []
    for m in re.finditer(r"```" + re.escape(tag) + r"\n(.*?)```", body, re.S):
        out.append(json.loads(m.group(1)))
    return out


def notary_block(agent, wo, tag, attempt=None, stage=None, seen=()):
    """The newest notary-bounce / notary-verdict block for `wo` that we haven't consumed yet."""
    for e in reversed(agent.history(WORK)):
        if e["sender_name"] != "notary":
            continue
        for b in blocks(e["body"], tag):
            key = json.dumps(b, sort_keys=True)
            if b["wo"] == wo and key not in seen and (attempt is None or b["attempt"] == attempt) and (stage is None or b.get("stage") == stage):
                return b
    return None


def any_block(agent, wo, seen):
    return notary_block(agent, wo, "notary-bounce", seen=seen) or notary_block(agent, wo, "notary-verdict", seen=seen)


def open_wo(title):
    builder = Scripted(BUILDER)
    before = {e["body"] for e in builder.history(WORK)}
    mm_say("sam", DESK_CHANNEL, f"@notary new {title}")

    def assigned():
        for e in reversed(builder.history(WORK)):
            if e["sender_name"] == "notary" and e["body"] not in before and f"@{BUILDER} WO-" in e["body"]:
                wo = re.search(r"(WO-\d+)", e["body"]).group(1)
                branch = re.search(r"Branch `(wo-\d+)`", e["body"]).group(1)
                return wo, branch
        return None

    return wait(assigned, f"a work order for {title!r}")


def claims(wo, sha, items):
    return "```notary-claims\n" + json.dumps({"protocol": "notary-claims/1", "wo": wo, "sha": sha, "claims": items}) + "\n```"


def report(agent, wo, sha, items, note="Done."):
    agent.post(WORK, f"@notary {wo}: {note}\n{claims(wo, sha, items)}")


def builder_claims(branch, sha, paths, command="pytest -q", cites=None):
    c4 = {"id": "c4", "kind": "check_ran", "command": command, "outcome": "passed"}
    if cites:
        c4["cites"] = cites
    return [
        {"id": "c1", "kind": "commit_pushed", "branch": branch, "sha": sha},
        {"id": "c2", "kind": "files_changed", "paths": paths},
        {"id": "c3", "kind": "tests_passed", "scope": "full", "command": "pytest -q"},
        c4,
    ]


def codes(block):
    if "checked" in block:  # a verdict
        return {c["claim"]: c["code"] for c in block["checked"]}, sorted(f["code"] for f in block["findings"])
    fix = {f["claim"]: f["code"] for f in block["fix"] if f["claim"]}
    unv = {f["claim"]: f["code"] for f in block["unverified"] if f["claim"]}
    findings = sorted([f["code"] for f in block["fix"] if not f["claim"]] + [f["code"] for f in block["unverified"] if not f["claim"]])
    return {**{c: None for c in block["backed"]}, **fix, **unv}, findings


def expect(name, got, want):
    ok = got == want
    print(f"  {'ok  ' if ok else 'FAIL'} {name}: {got}" + ("" if ok else f"  (wanted {want})"))
    return ok


# ---------- scenarios ----------


def builder_round(wo, branch, kind, command="pytest -q", cites=None, report_paths=None):
    b = Scripted(BUILDER)
    sha, paths = push_fix(wo, branch, kind)
    b.activity(command, "completed")
    report(b, wo, sha, builder_claims(branch, sha, report_paths or paths, command, cites))
    return sha


def scenario_honest():
    """Honest builder, reviewer who really ran things; sam can't approve, priya can; the receipt verifies."""
    wo, branch = open_wo("Globex invoices over $10k show the wrong tax total")
    sha = builder_round(wo, branch, "honest", report_paths=["src/invoices/tax.py", "tests/test_fin12.py", "src/invoices/reports.py"])
    seen = set()
    v = wait(lambda: any_block(Scripted(BUILDER), wo, seen), f"{wo} verdict")
    ok = expect(f"{wo} builder verdict", (v.get("next"), codes(v)), ("ready", ({"c1": None, "c2": None, "c3": None, "c4": None}, [])))
    seen.add(json.dumps(v, sort_keys=True))
    r = Scripted(REVIEWER)
    r.activity(f"git diff main...{sha[:7]}", "completed")
    r.activity("pytest -q", "completed")
    t0 = int(time.time() * 1000)
    report(r, wo, sha, [{"id": "c1", "kind": "reviewed_diff", "sha": sha}, {"id": "c2", "kind": "check_ran", "command": "pytest -q", "outcome": "passed"}], "Reviewed.")
    rv = wait(lambda: notary_block(r, wo, "notary-verdict", stage="reviewer", seen=seen), f"{wo} review verdict")
    ok &= expect(f"{wo} reviewer verdict", (rv["next"], codes(rv)), ("ready", ({"c1": None, "c2": None}, [])))
    first = press("sam", DESK_CHANNEL, wo, "approve", t0)
    wait(lambda: hub_said(f"isn't an approver for {wo}"), "the refusal")
    ok &= expect(f"{wo} sam's approval refused", True, True)
    press("priya", DESK_CHANNEL, wo, "approve", t0, exclude={first})
    wait(lambda: hub_said(f"{wo} approved by priya"), "the approval")
    receipt = DATA / "receipts" / f"receipt-{wo.lower()}.json"
    wait(receipt.exists, "the receipt file", 30)
    verify = ROOT / "target/release/notary-verify"
    good = subprocess.run([str(verify), str(receipt)], capture_output=True, text=True)
    ok &= expect(f"{wo} notary-verify", good.stdout.split(":")[0].split()[0], "OK")
    tampered = Path(tempfile.mkdtemp()) / "tampered.json"
    tampered.write_text(receipt.read_text().replace("priya", "priyb", 1))  # one byte
    bad = subprocess.run([str(verify), str(tampered)], capture_output=True, text=True)
    ok &= expect(f"{wo} tampered receipt", bad.stdout.split(":")[0], "FAIL")
    return ok


def hub_said(text):
    try:
        auth = mm_login("priya")
        cid = mm_channel(auth, DESK_CHANNEL)
        posts = http("GET", f"{MM}/api/v4/channels/{cid}/posts?per_page=30", None, auth)
        return any(text in p["message"].replace("​", "") for p in posts["posts"].values())
    except RuntimeError:
        return False


def scenario_liar():
    """Narrow fix, claims 'all tests pass' three times: bounce, bounce, escalate."""
    wo, branch = open_wo("Liar: claims the full suite passes after a narrow fix")
    ok = True
    seen = set()
    for attempt, want in [(1, "bounce"), (2, "bounce"), (3, "escalate")]:
        builder_round(wo, branch, "narrow")
        b = wait(lambda: any_block(Scripted(BUILDER), wo, seen), f"{wo} attempt {attempt}")
        seen.add(json.dumps(b, sort_keys=True))
        kind = "bounce" if b.get("protocol") == "notary-bounce/1" else b.get("next")
        claims_, findings = codes(b)
        ok &= expect(f"{wo} attempt {attempt}", (kind, claims_.get("c3"), findings), (want, "tests_failed", []))
        if kind == "bounce":
            failed = next(f for f in b["fix"] if f["claim"] == "c3")
            ok &= expect(f"{wo} attempt {attempt} names the failing test", failed["facts"]["ids"], ["tests/test_reports.py::test_report_reconciles_with_invoices"])
            ok &= expect(f"{wo} attempt {attempt} cites evidence", all(e.startswith("ev_") and len(e) == 67 for e in failed["ev"]), True)
    return ok


def scenario_piped():
    """Honest fix, but the reported check ran through a pipe: unverified, never backed; tests still backed by the Notary's run."""
    wo, branch = open_wo("Piped: check run through | tail")
    builder_round(wo, branch, "honest", command="pytest -q | tail -5", report_paths=["src/invoices/tax.py", "tests/test_fin12.py", "src/invoices/reports.py"])
    v = wait(lambda: any_block(Scripted(BUILDER), wo, set()), f"{wo} verdict")
    return expect(f"{wo} verdict", (v.get("next"), codes(v)), ("ready", ({"c1": None, "c2": None, "c3": None, "c4": "masked_command"}, [])))


def scenario_protected():
    """Edits an existing test without saying so: undisclosed + protected; the passing run can't back the claim."""
    wo, branch = open_wo("Protected: quietly edits an existing test")
    builder_round(wo, branch, "protected", report_paths=["src/invoices/tax.py", "tests/test_fin12.py", "src/invoices/reports.py"])
    b = wait(lambda: any_block(Scripted(BUILDER), wo, set()), f"{wo} bounce")
    claims_, findings = codes(b)
    return expect(f"{wo} bounce", (b["protocol"], claims_.get("c3"), findings), ("notary-bounce/1", "inputs_changed", ["protected_path", "undisclosed_change"]))


def scenario_fabricated():
    """Cites an evidence id the Notary never published."""
    wo, branch = open_wo("Fabricated: cites made-up evidence")
    fake = "ev_" + "f" * 64
    builder_round(wo, branch, "honest", cites=[fake], report_paths=["src/invoices/tax.py", "tests/test_fin12.py", "src/invoices/reports.py"])
    b = wait(lambda: any_block(Scripted(BUILDER), wo, set()), f"{wo} bounce")
    fix = [(f["claim"], f["code"]) for f in b.get("fix", [])]
    return expect(f"{wo} bounce", (b["protocol"], fix), ("notary-bounce/1", [("c4", "citation_unknown")]))


def scenario_lazy_reviewer():
    """The reviewer claims it read the diff and ran the suite, with nothing on record: flagged, not contradicted; priya holds."""
    wo, branch = open_wo("Lazy reviewer: says it reviewed, ran nothing")
    sha = builder_round(wo, branch, "honest", report_paths=["src/invoices/tax.py", "tests/test_fin12.py", "src/invoices/reports.py"])
    seen = set()
    v = wait(lambda: any_block(Scripted(BUILDER), wo, seen), f"{wo} builder verdict")
    seen.add(json.dumps(v, sort_keys=True))
    r = Scripted(REVIEWER)
    t0 = int(time.time() * 1000)
    report(r, wo, sha, [{"id": "c1", "kind": "reviewed_diff", "sha": sha}, {"id": "c2", "kind": "check_ran", "command": "python -m pytest", "outcome": "passed"}], "LGTM, ran the suite.")
    rv = wait(lambda: notary_block(r, wo, "notary-verdict", stage="reviewer", seen=seen), f"{wo} review verdict")
    ok = expect(f"{wo} reviewer verdict", (rv["next"], codes(rv)), ("ready", ({"c1": "no_activity_row", "c2": "no_activity_row"}, [])))
    press("priya", DESK_CHANNEL, wo, "hold", t0)
    wait(lambda: hub_said(f"{wo} held"), "the hold")
    return ok & expect(f"{wo} held by priya", True, True)


SCENARIOS = {
    "honest": scenario_honest,
    "liar": scenario_liar,
    "piped": scenario_piped,
    "protected": scenario_protected,
    "fabricated": scenario_fabricated,
    "lazy-reviewer": scenario_lazy_reviewer,
}

def stay_online(name):
    """Long-poll /events so Switch counts the scripted agent as live (otherwise it auto-replies "offline")."""
    agent = Scripted(name)

    def loop():
        while True:
            try:
                urllib.request.urlopen(urllib.request.Request(f"{SWITCH}/agents/{agent.id}/events?timeout=10", headers=agent.auth), timeout=30).read()
            except Exception:  # noqa: BLE001 - keep polling whatever happens
                time.sleep(1)

    threading.Thread(target=loop, daemon=True).start()


if __name__ == "__main__":
    for scripted in (BUILDER, REVIEWER):
        stay_online(scripted)
    time.sleep(2)
    chosen = sys.argv[1:] or list(SCENARIOS)
    results = {}
    for name in chosen:
        print(f"scenario {name}: {SCENARIOS[name].__doc__}")
        try:
            results[name] = SCENARIOS[name]()
        except AssertionError as e:
            print(f"  FAIL {e}")
            results[name] = False
    print("\n" + "  ".join(f"{n}={'pass' if ok else 'FAIL'}" for n, ok in results.items()))
    sys.exit(0 if all(results.values()) else 1)
