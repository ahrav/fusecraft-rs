#!/usr/bin/env bash
# poll_pr.sh — Background PR comment polling loop.
#
# Usage: poll_pr.sh <owner/repo> <pr-number> <watermark-iso8601> [interval-seconds]
#
# Polls for new inline and issue-level comments on the given PR.
# Writes machine-readable outputs that the consuming agent reads:
#
#   /tmp/pr_<repo_key>_<pr>_state           — "OPEN", "MERGED", or "CLOSED"
#   /tmp/pr_<repo_key>_<pr>_pending.jsonl   — one JSON object per new comment (append)
#   /tmp/pr_<repo_key>_<pr>_watermark       — ISO-8601 timestamp of the newest comment seen
#   /tmp/pr_<repo_key>_<pr>_seen_ids        — set of already-reported comment IDs
#   /tmp/pr_<repo_key>_<pr>_poll.log        — human-readable log
#
# Tracking model:
#   Comments are identified as "new" when their numeric id is not present in
#   the seen-ids file. Timestamp-only tracking (the earlier design) dropped
#   comments that shared a second-precision `created_at` with the watermark
#   — e.g., a bot posting three inline comments in a single review round.
#   The id-set model is resilient to:
#     * same-second batches (reviews that post N comments in one POST)
#     * clock skew or out-of-order delivery
#     * deleted-and-reposted comments (new ids generated)
#     * retries that replay the same ids
#
# The consuming agent is responsible for:
#   1. Draining /tmp/pr_<repo_key>_<pr>_pending.jsonl each cycle
#      (atomic mv-rename, then read, then delete)
#   2. Checking /tmp/pr_<repo_key>_<pr>_state for MERGED/CLOSED exit condition
#   3. After a commit+push, doing a full API re-fetch and reconciling against
#      its own set of replied ids (ack filters miss new-round reviews that
#      contain "thanks"/"confirmed" in their body).

set -euo pipefail

REPO="${1:?usage: poll_pr.sh <owner/repo> <pr-number> <watermark> [interval]}"
PR="${2:?usage: poll_pr.sh <owner/repo> <pr-number> <watermark> [interval]}"
WATERMARK="${3:?usage: poll_pr.sh <owner/repo> <pr-number> <watermark> [interval]}"
INTERVAL="${4:-60}"

# Validate INTERVAL — a non-numeric or non-positive value would cause `sleep`
# to fail, and `set -e` would silently terminate the monitor.
if ! [[ "$INTERVAL" =~ ^[0-9]+$ ]] || [[ "$INTERVAL" -le 0 ]]; then
    echo "ERROR: interval must be a positive integer number of seconds (got: $INTERVAL)" >&2
    exit 2
fi

# Scope file names to repo+PR so multiple monitors don't collide.
REPO_KEY=$(printf '%s' "$REPO" | tr '/:' '__')
PREFIX="/tmp/pr_${REPO_KEY}_${PR}"

LOG="${PREFIX}_poll.log"
STATE_FILE="${PREFIX}_state"
PENDING_FILE="${PREFIX}_pending.jsonl"
WATERMARK_FILE="${PREFIX}_watermark"
SEEN_IDS_FILE="${PREFIX}_seen_ids"

# Resolve the repo owner's login so we can exclude self-replies.
SELF=$(gh api user --jq '.login' 2>/dev/null || echo "__unknown__")

echo "OPEN" > "$STATE_FILE"
echo "$WATERMARK" > "$WATERMARK_FILE"
: > "$PENDING_FILE"

# Seed the seen-ids file with every comment currently on the PR whose
# `created_at <= WATERMARK` so the agent's initial pass (which consumed
# those comments already) does not get them re-delivered. Any bot comment
# that posts after the agent's initial fetch is new and will be captured
# on the first poll cycle regardless of whether it shares a second with
# the watermark.
: > "$SEEN_IDS_FILE"
{
    gh api --paginate "repos/$REPO/pulls/$PR/comments" 2>/dev/null \
        | jq -s 'add // []' \
        | jq -r --arg wm "$WATERMARK" '
            ($wm | fromdateiso8601) as $wm_epoch
            | .[]
            | select((.created_at | fromdateiso8601) <= $wm_epoch)
            | .id
          '
    gh api --paginate "repos/$REPO/issues/$PR/comments" 2>/dev/null \
        | jq -s 'add // []' \
        | jq -r --arg wm "$WATERMARK" '
            ($wm | fromdateiso8601) as $wm_epoch
            | .[]
            | select((.created_at | fromdateiso8601) <= $wm_epoch)
            | .id
          '
} >> "$SEEN_IDS_FILE" 2>/dev/null || true

seeded=$(wc -l < "$SEEN_IDS_FILE" 2>/dev/null | tr -d ' ')
echo "[$(date -Iseconds)] poll_pr.sh started — repo=$REPO pr=$PR interval=${INTERVAL}s self=$SELF seeded_ids=$seeded watermark=$WATERMARK" | tee "$LOG"

# Emit a comment to pending.jsonl if its id is not already in seen-ids.
# Arguments:
#   $1 — type label ("INLINE_COMMENT" or "ISSUE_COMMENT")
#   $2 — JSON array (string) of comments from the API
emit_new_comments() {
    local type_label="$1"
    local json="$2"

    # jq expression differs slightly: inline has path/line, issue does not.
    local projection
    if [[ "$type_label" == "INLINE_COMMENT" ]]; then
        projection='{type:"INLINE_COMMENT",id:.id,user:.user.login,path:.path,line:(.line // .original_line),ts:.created_at,body:.body}'
    else
        projection='{type:"ISSUE_COMMENT",id:.id,user:.user.login,ts:.created_at,body:.body}'
    fi

    # Pass the seen-ids file to jq as a newline-separated input built into
    # a set. This is O(1) per-comment lookup and avoids spawning a shell
    # loop per comment.
    jq -c --arg self "$SELF" --rawfile seen "$SEEN_IDS_FILE" "
      ( \$seen | split(\"\n\") | map(select(length>0) | tonumber) | reduce .[] as \$i ({}; .[\$i|tostring] = true) ) as \$seen_set
      | .[]
      | select(.user.login != \$self and (\$seen_set[(.id|tostring)] // false | not))
      | $projection
    " <<<"$json"
}

while true; do
    # ── Check PR state ──────────────────────────────────────────────
    state=$(gh pr view "$PR" --repo "$REPO" --json state --jq '.state' 2>/dev/null || echo "ERROR")
    case "$state" in
        MERGED)
            echo "MERGED" > "$STATE_FILE"
            echo "[$(date -Iseconds)] PR #$PR MERGED — exiting." | tee -a "$LOG"
            exit 0 ;;
        CLOSED)
            echo "CLOSED" > "$STATE_FILE"
            echo "[$(date -Iseconds)] PR #$PR CLOSED — exiting." | tee -a "$LOG"
            exit 0 ;;
        ERROR)
            echo "[$(date -Iseconds)] WARNING: failed to fetch PR state" >> "$LOG"
            sleep "$INTERVAL"; continue ;;
    esac

    # ── Snapshot comments (single API call per kind, pages merged) ──
    inline_json=$(gh api --paginate "repos/$REPO/pulls/$PR/comments" 2>/dev/null | jq -s 'add // []' || echo "[]")
    issue_json=$(gh api --paginate "repos/$REPO/issues/$PR/comments" 2>/dev/null | jq -s 'add // []' || echo "[]")

    # ── Emit any id not in seen-ids ────────────────────────────────
    new_inline=$(emit_new_comments INLINE_COMMENT "$inline_json")
    new_issue=$(emit_new_comments ISSUE_COMMENT "$issue_json")

    inline_count=0
    issue_count=0
    [[ -n "$new_inline" ]] && inline_count=$(printf '%s\n' "$new_inline" | grep -c '^' || true)
    [[ -n "$new_issue"  ]] && issue_count=$(printf '%s\n' "$new_issue"  | grep -c '^' || true)

    if [[ "$inline_count" -gt 0 ]] || [[ "$issue_count" -gt 0 ]]; then
        echo "[$(date -Iseconds)] NEW_COMMENTS inline=$inline_count issue=$issue_count" | tee -a "$LOG"

        # Append new comments to the pending file.
        [[ -n "$new_inline" ]] && printf '%s\n' "$new_inline" >> "$PENDING_FILE"
        [[ -n "$new_issue"  ]] && printf '%s\n' "$new_issue"  >> "$PENDING_FILE"

        # Record these ids as seen so they aren't re-delivered next cycle.
        {
            [[ -n "$new_inline" ]] && printf '%s\n' "$new_inline" | jq -r '.id'
            [[ -n "$new_issue"  ]] && printf '%s\n' "$new_issue"  | jq -r '.id'
        } >> "$SEEN_IDS_FILE"

        # Advance watermark to the newest ts we just emitted (informational
        # for the agent; the dedup contract is the id set, not the ts).
        new_wm=$({
            [[ -n "$new_inline" ]] && printf '%s\n' "$new_inline" | jq -r '.ts'
            [[ -n "$new_issue"  ]] && printf '%s\n' "$new_issue"  | jq -r '.ts'
            cat "$WATERMARK_FILE" 2>/dev/null
        } | sort -r | head -1)
        echo "$new_wm" > "$WATERMARK_FILE"
    else
        echo "[$(date -Iseconds)] no new comments — PR is $state" >> "$LOG"
    fi

    sleep "$INTERVAL"
done
