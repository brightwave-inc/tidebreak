#!/usr/bin/env python3
"""Summarize gh-pr-status output for one PR: checks, comments, reviews, inline comments."""
import json, subprocess, sys
repo, n = sys.argv[1], sys.argv[2]
extra = sys.argv[3:]
raw = subprocess.run(["gh-pr-status", repo, n, *extra], capture_output=True, text=True).stdout
raw = "".join(ch for ch in raw if ch >= " " or ch in "\n\t")
d = json.loads(raw, strict=False)
pr = d["pr"]
print(f"state={pr['state']} merged={pr.get('merged')} head={pr['head']['sha'][:9]} mergeable_state={pr.get('mergeable_state')} draft={pr.get('draft')}")
for c in d.get("check_runs", {}).get("check_runs", []):
    print(f"  check {c['name']}\t{c['status']}\t{c.get('conclusion') or '-'}")
for c in d.get("issue_comments", []):
    print(f"--- comment {c['user']['login']} {c['created_at']}\n{c['body'][:3000]}")
for r in d.get("reviews", []):
    print(f"--- review {r['user']['login']} [{r['state']}] {r.get('commit_id','')[:9]}\n{(r.get('body') or '')[:3000]}")
for c in d.get("review_comments", []):
    print(f"--- inline {c['user']['login']} {c['path']}:{c.get('line')} {c.get('commit_id','')[:9]}\n{c['body'][:2000]}")
