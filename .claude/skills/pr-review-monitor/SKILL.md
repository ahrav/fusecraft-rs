---
name: pr-review-monitor
description: Fully autonomous PR review comment monitor that triages, responds to, and fixes review comments using the verify-first /pr-comment-response workflow, then polls for new comments until the PR is merged or closed. Use when the user says "monitor this PR", "watch for PR comments", "address PR comments until merged", or invokes /pr-review-monitor <PR-number>.
---

# PR Review Monitor

Respond to all current review comments, then poll for new ones in a
foreground loop until the PR is merged or closed.

Invoke with: `/pr-review-monitor <PR-number>`

## Phase 1: Initial Comment Response

1. **Fetch** all inline and issue-level comments.
2. **Triage** into categories (bug, invariant, style, nitpick, refactor, question).
3. **Verify-first** for every correctness claim.
4. **Apply** minimal fixes supported by evidence.
5. **Run quality gates** (`cargo fmt && cargo check && cargo clippy --all-targets -- -D warnings && cargo test --lib`).
6. **Commit and push** all fixes in a single commit.
7. **Post replies** to every comment.
8. **Record the watermark** — the ISO-8601 timestamp of the newest
   non-self comment processed.

## Phase 2: Foreground Poll Loop

`poll_pr.sh` runs as a **long-lived background poller** that writes state
to files under `/tmp`. The agent drives a foreground loop that drains
those files each iteration. No extra daemons, no nohup, no PID files —
the agent is still the consumer and decision-maker.

```text
watermark = <from Phase 1>
launch    : .claude/skills/pr-review-monitor/scripts/poll_pr.sh <owner/repo> <PR> <watermark> 65 &

loop:
    1. sleep 65
    2. state = read /tmp/pr_<repo_key>_<pr>_state
    3. if state == MERGED or CLOSED: exit
    4. atomically rename pending.jsonl → pending.jsonl.inflight, read, delete
    5. if no new comments: goto 1
    6. Filter out bot acknowledgments
    7. Triage + verify-first + fix + reply for substantive comments
    8. watermark = read /tmp/pr_<repo_key>_<pr>_watermark
    9. Goto 1
```

### poll_pr.sh

Long-running poller. Invoke once with the starting watermark and leave it
in the background:

```bash
.claude/skills/pr-review-monitor/scripts/poll_pr.sh owner/repo 123 "$watermark" 65 &
```

Writes (all scoped by `REPO_KEY=owner_repo` + PR number):

- `/tmp/pr_<repo_key>_<pr>_state` — `OPEN` / `MERGED` / `CLOSED`
- `/tmp/pr_<repo_key>_<pr>_pending.jsonl` — one JSON object per new
  comment (append-only between agent drains)
- `/tmp/pr_<repo_key>_<pr>_watermark` — ISO-8601 timestamp of the newest
  comment observed
- `/tmp/pr_<repo_key>_<pr>_seen_ids` — dedup set (script-managed)
- `/tmp/pr_<repo_key>_<pr>_poll.log` — human-readable log

The consumer (the agent) owns the drain/reply/commit cycle; the poller
only detects and reports.

### Bot Acknowledgments

Do not reply to bot comments that are just confirmations ("thanks",
"confirmed", "looks good", etc.) with no new claim or question.

## Autonomous Operation

Fully autonomous — no user prompts at any severity level:

| Severity | Action |
|----------|--------|
| Critical, High | Execute |
| Medium | Execute if verifiable, contained, and gates pass |
| Low, Nit | Apply if mechanical and risk-free |

## Reply Mechanics

```bash
# Inline review comments:
gh api repos/{owner}/{repo}/pulls/{pr}/comments \
    -f body='...' -F in_reply_to=<comment_id>

# Issue-level comments:
gh api repos/{owner}/{repo}/issues/{pr}/comments \
    -f body='...'
```

## Exit

When `/tmp/pr_<repo_key>_<pr>_state` reads `MERGED` or `CLOSED`, stop.
