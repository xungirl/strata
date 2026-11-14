#!/usr/bin/env bash
# Negative controls for the Raft simulation tests: inject a known bug into
# crates/strata-raft/src/core.rs, run the simulation, expect a failure,
# restore the file. Output: results/stage8/mutations.txt
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
F=crates/strata-raft/src/core.rs
OUT=results/stage8/mutations.txt
BK=$(mktemp); LOG=$(mktemp)
cp $F "$BK"
trap 'cp "$BK" $F' EXIT
: > $OUT
mutate() { # name perl-substitution
  local name=$1 expr=$2
  cp "$BK" $F
  perl -0pi -e "$expr" $F
  if cmp -s "$BK" $F; then echo "$name: MUTATION DID NOT APPLY" | tee -a $OUT; return; fi
  if cargo test -q --release -p strata-raft --test sim_tests > "$LOG" 2>&1; then
    echo "$name: NOT DETECTED (tests passed)" | tee -a $OUT
  else
    msg=$(grep -m1 -E "panicked|assert" "$LOG" | sed 's/^ *//' | cut -c1-160)
    echo "$name: detected -- $msg" | tee -a $OUT
  fi
}
mutate "no-figure8-rule (commit entries of older terms by counting replicas)" 's/candidate > self\.commit && self\.term_at\(candidate\) == self\.hs\.term/candidate > self.commit/'
mutate "no-up-to-date-check (vote for any candidate)" 's/let up_to_date = \(last_term, last_index\) >= \(self\.last_term\(\), self\.last_index\(\)\);/let up_to_date = true;/'
mutate "vote-twice (ignore voted_for)" 's/let can_vote = self\.hs\.voted_for\.is_none_or\(\|v\| v == from\);/let can_vote = true;/'
mutate "stale-read-index (use commit index at request time)" 's/self\.ready\.reads\.push\(\(id, idx\.max\(self\.commit\)\)\);/self.ready.reads.push((id, idx));/'
mutate "read-without-quorum (serve ReadIndex immediately)" 's/acks\.len\(\) >= quorum/acks.len() >= 1/'
mutate "follower-commit-unbounded (commit = leader commit)" 's/let new_commit = commit\.min\(last_new\);/let new_commit = commit.min(self.last_index());/'
