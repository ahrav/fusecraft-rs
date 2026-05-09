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

Run the loop **in the foreground as the main process**. No background
daemons, no nohup, no PID files. The agent IS the poller.

```
watermark = <from Phase 1>

loop:
    1. sleep 65
    2. output = call poll_pr.sh <owner/repo> <PR> <watermark>
    3. Parse STATE line → if MERGED or CLOSED: exit
    4. Parse COMMENT lines → if none: goto 1
    5. Filter out bot acknowledgments
    6. Triage + verify-first + fix + reply for substantive comments
    7. Parse WATERMARK line → update watermark
    8. Goto 1
```

### poll_pr.sh

Helper that fetches state and new comments in one call:

```bash
output=$(.claude/skills/pr-review-monitor/scripts/poll_pr.sh owner/repo 123 "$watermark")
```

Prints to stdout:
- `STATE OPEN` / `MERGED` / `CLOSED`
- `COMMENT {json}` — one per new comment
- `WATERMARK <iso8601>` — pass back next call

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

When poll_pr.sh returns `STATE MERGED` or `STATE CLOSED`, stop.
