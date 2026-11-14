First chaos run (git 3 commits before the async applier), superseded.

What went wrong (all found from these logs):
1. Committed entries were applied to the index while holding the Raft lock.
   A restarted node replaying thousands of entries (one WAL fsync each)
   could not vote or answer for seconds. Kills came every ~2 s, so the next
   leader kill often left a still-catching-up node as the only partner:
   "failover" times grew with the log (p50 3.3 s in run 1, 6.8 s in run 2,
   up to 17 s) because they measured catch-up, not election.
2. The final check counted any non-200 consistent read as "lost". Run 1
   reported LOST=2713 while plain reads on all three replicas found every
   one of those ids (replicas missing = [0, 0, 0]): the consistent reads
   had timed out against a busy leader, nothing was lost.
3. Run 3 aborted in the final check on an HTTP timeout to a busy node.

Fixes: dedicated applier thread fed by a channel (Raft keeps running while
the state machine catches up; runs of upserts are applied in one batch);
the harness waits for all replicas to catch up before the next kill and
reports the catch-up time separately; final checks retry and distinguish
"not found" from "no answer".
