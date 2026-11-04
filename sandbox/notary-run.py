"""Run the acceptance check and write /out/result.json (protocol notary-run/1).

The Notary passes the argv (JSON) and a fresh run id; this echoes them back with the sha, so the
Notary can tell the result belongs to this run. It parses pytest's JUnit XML, never its prose.
"""
import json, os, subprocess, sys
import xml.etree.ElementTree as ET

argv = json.loads(os.environ["NOTARY_ARGV"])
junit = "/tmp/junit.xml"
if argv and argv[0] in ("pytest", "py.test"):
    argv = [sys.executable, "-m", "pytest", "-p", "no:cacheprovider", f"--junitxml={junit}", *argv[1:]]
code = subprocess.run(argv, cwd="/src").returncode

counts = {"collected": 0, "passed": 0, "failed": 0, "errors": 0, "skipped": 0}
failed_ids = []
if os.path.exists(junit):
    root = ET.parse(junit).getroot()
    for suite in ([root] if root.tag == "testsuite" else root.iter("testsuite")):
        for case in suite.iter("testcase"):
            counts["collected"] += 1
            kids = {child.tag for child in case}
            if "failure" in kids or "error" in kids:
                counts["failed" if "failure" in kids else "errors"] += 1
                path = case.get("file") or case.get("classname", "").replace(".", "/") + ".py"
                failed_ids.append(f"{path}::{case.get('name')}")
            elif "skipped" in kids:
                counts["skipped"] += 1
            else:
                counts["passed"] += 1

result = {
    "protocol": "notary-run/1",
    "run_id": os.environ["NOTARY_RUN_ID"],
    "sha": os.environ["NOTARY_SHA"],
    "exit_code": code,
    "counts": counts,
    "failed_ids": failed_ids[:50],
    "failed_truncated": len(failed_ids) > 50,
}
tmp = "/out/result.json.tmp"
with open(tmp, "w") as f:
    json.dump(result, f)
os.replace(tmp, "/out/result.json")
sys.exit(0)
