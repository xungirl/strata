### Leader-kill chaos test (3-node shard, SIGKILL the leader, 25 kills per run, 3 runs)

| run | kills | acked writes (ids) | lost acked ids | unverified | read-after-ack (consistent) violations / checks | read-after-failover violations / checks | follower plain-read misses / checks | failover p50 / max ms | catch-up p50 / max ms | replicas missing acked ids |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 25 | 17620 | 0 | 0 | 0 / 1749 | 0 / 1250 | 54 / 1063 | 251 / 512 | 257 / 552 | [0, 0, 0] |
| 2 | 25 | 17700 | 0 | 0 | 0 / 1756 | 0 / 1250 | 42 / 1243 | 233 / 693 | 261 / 521 | [0, 0, 0] |
| 3 | 25 | 17810 | 0 | 0 | 0 / 1772 | 0 / 1250 | 40 / 1154 | 268 / 782 | 258 / 567 | [0, 0, 0] |
| **total** | 75 | 53130 | 0 | 0 | 0 / 5277 | 0 / 3750 | 136 / 3460 | | | |

### Failover time distribution (SIGKILL of the leader -> next write acknowledged by another replica)

| quantity | n | min | p50 | p90 | max | mean ± std | per-run p50 (mean ± std of 3) |
|---|---|---|---|---|---|---|---|
| failover ms | 75 | 182 | 261 | 448 | 782 | 288 ± 117 | 251 ± 18 |
| restarted node catch-up ms | 75 | 116 | 257 | 390 | 567 | 276 ± 110 | 258 ± 2 |

### Mutation tests (inject a known Raft bug, expect the simulation tests to fail)

| injected bug | result |
|---|---|
| no-figure8-rule (commit entries of older terms by counting replicas) | detected (crates/strata-raft/tests/sim_tests.rs:473:17) |
| no-up-to-date-check (vote for any candidate) | detected (crates/strata-raft/tests/sim_tests.rs:534:5) |
| vote-twice (ignore voted_for) | detected (crates/strata-raft/tests/sim_tests.rs:160:17) |
| stale-read-index (use commit index at request time) | detected (crates/strata-raft/tests/sim_tests.rs:150:17) |
| read-without-quorum (serve ReadIndex immediately) | detected (crates/strata-raft/tests/sim_tests.rs:150:17) |
| follower-commit-unbounded (commit = leader commit) | detected (crates/strata-raft/tests/sim_tests.rs:138:17) |

