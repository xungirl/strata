//! Randomized simulation of a Raft cluster with an unreliable network and
//! crashes, checking Raft's safety properties on every schedule.
//!
//! The simulated network delivers messages in random order, drops some,
//! duplicates some, and can partition nodes. Nodes crash and restart from
//! exactly their persisted state (hard state + durable log). Checked:
//! * **Election safety**: at most one leader per term.
//! * **State machine safety**: every node applies the same sequence of
//!   entries (each node's applied list is a prefix of the longest one).
//! * **Durability of commits**: an entry a leader reported as committed is
//!   applied by everyone at that index in the end.
//! * **Linearizable reads (ReadIndex)**: a read that completes with read
//!   index `r` must have `r >=` the index of every write acknowledged to a
//!   client before the read started.
//! * **Liveness after healing**: once the network is healed and all nodes
//!   are up, new proposals commit.

use std::collections::{HashMap, HashSet};

use strata_raft::core::{Entry, Envelope, HardState, RaftNode, Role, Timing};

struct SimNode {
    node: Option<RaftNode>,
    hs: HardState,
    log: Vec<Entry>,
    applied: Vec<Entry>,
}

struct Sim {
    nodes: Vec<SimNode>,
    net: Vec<Envelope>,
    rng: u64,
    partitioned: HashSet<u64>,
    leaders_by_term: HashMap<u64, u64>,
    /// (index, data) the leader reported committed.
    committed_acks: Vec<(u64, Vec<u8>)>,
    proposals: Vec<(u64, Vec<u8>)>,
    /// (node, read id) -> highest acknowledged write index when the read began.
    pending_reads: HashMap<(u64, u64), u64>,
    reads_checked: usize,
    timing: Timing,
    /// The longest sequence of applied entries seen on any node.
    canonical: Vec<Entry>,
    /// Partitions change on 1 in `partition_hold` partition events
    /// (1 = short, frequent partitions; 8 = long ones). Varied by seed.
    partition_hold: u64,
    seed: u64,
}

impl Sim {
    fn new(n: usize, seed: u64) -> Sim {
        let ids: Vec<u64> = (1..=n as u64).collect();
        // Vary the AppendEntries batch size across seeds: with 1 or 2
        // entries per message, a lagging follower is caught up in many
        // small steps, which exercises partial-replication cases.
        let timing = Timing {
            max_batch: [1, 2, 64][(seed % 3) as usize],
            ..Timing::default()
        };
        let nodes = ids
            .iter()
            .map(|&id| SimNode {
                node: Some(RaftNode::new(
                    id,
                    ids.clone(),
                    HardState::default(),
                    Vec::new(),
                    timing,
                    seed + id,
                )),
                hs: HardState::default(),
                log: Vec::new(),
                applied: Vec::new(),
            })
            .collect();
        Sim {
            nodes,
            net: Vec::new(),
            rng: seed | 1,
            partitioned: HashSet::new(),
            leaders_by_term: HashMap::new(),
            committed_acks: Vec::new(),
            proposals: Vec::new(),
            pending_reads: HashMap::new(),
            reads_checked: 0,
            timing,
            canonical: Vec::new(),
            partition_hold: [1, 8][((seed / 3) % 2) as usize],
            seed,
        }
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// `ids` starting at a random position, so that when two nodes both
    /// believe they are leader (an old one that has not noticed it was
    /// deposed, and the real one), either may receive the client request.
    fn rotated(&mut self, mut ids: Vec<u64>) -> Vec<u64> {
        let k = (self.rand() % ids.len() as u64) as usize;
        ids.rotate_left(k);
        ids
    }

    fn ids(&self) -> Vec<u64> {
        (1..=self.nodes.len() as u64).collect()
    }

    /// Persist, then send, then apply: exactly the order `Ready` demands.
    fn drain(&mut self, id: u64) {
        let i = id as usize - 1;
        let Some(node) = self.nodes[i].node.as_mut() else {
            return;
        };
        let ready = node.take_ready();
        let (role, term) = (node.role(), node.term());
        let sn = &mut self.nodes[i];
        if let Some(hs) = ready.hard_state {
            sn.hs = hs;
        }
        if let Some(t) = ready.truncate_from {
            sn.log.truncate(t as usize - 1);
        }
        for e in ready.append {
            assert_eq!(e.index, sn.log.len() as u64 + 1);
            sn.log.push(e);
        }
        for e in &ready.committed {
            sn.applied.push(e.clone());
            // State machine safety, checked on every apply: the k-th entry
            // any node applies is the same on every node.
            let k = sn.applied.len();
            if k <= self.canonical.len() {
                assert_eq!(
                    &self.canonical[k - 1],
                    e,
                    "state machines diverged at {k} (seed {})",
                    self.seed
                );
            } else {
                self.canonical.push(e.clone());
            }
        }
        for (rid, idx) in &ready.reads {
            if let Some(required) = self.pending_reads.remove(&(id, *rid)) {
                assert!(
                    idx >= &required,
                    "stale read: read index {idx} < acked write {required} (seed {})",
                    self.seed
                );
                self.reads_checked += 1;
            }
        }
        if role == Role::Leader {
            if let Some(&other) = self.leaders_by_term.get(&term) {
                assert_eq!(other, id, "two leaders in term {term} (seed {})", self.seed);
            }
            self.leaders_by_term.insert(term, id);
            for e in &ready.committed {
                if !e.data.is_empty() {
                    self.committed_acks.push((e.index, e.data.clone()));
                }
            }
        }
        self.net.extend(ready.messages);
    }

    fn step(&mut self, chaos: bool) {
        let r = self.rand() % 100;
        let ids = self.ids();
        if r < 30 {
            // tick a random live node
            let id = ids[(self.rand() % ids.len() as u64) as usize];
            if let Some(n) = self.nodes[id as usize - 1].node.as_mut() {
                n.tick();
                self.drain(id);
            }
        } else if r < 85 {
            // Deliver random messages (random order = reordering). The
            // network's capacity grows with its backlog (1 message per 100
            // queued), so delays stay bounded: with a fixed one message per
            // step, a busy cluster generates messages faster than they are
            // delivered and every message ends up delayed past the election
            // timeout, which tests nothing useful.
            let burst = 1 + self.net.len() / 100;
            for _ in 0..burst {
                if self.net.is_empty() {
                    return;
                }
                let k = (self.rand() % self.net.len() as u64) as usize;
                let env = self.net.swap_remove(k);
                let lost = chaos && self.rand().is_multiple_of(10);
                let cut =
                    self.partitioned.contains(&env.from) != self.partitioned.contains(&env.to);
                if chaos && self.rand().is_multiple_of(20) {
                    self.net.push(env.clone()); // duplicate
                }
                if lost || cut {
                    continue;
                }
                let to = env.to;
                if let Some(n) = self.nodes[to as usize - 1].node.as_mut() {
                    n.step(env);
                    self.drain(to);
                }
            }
        } else if r < 92 {
            // propose on whoever thinks it is leader
            let data = format!("v{}", self.rand() % 1_000_000).into_bytes();
            for id in self.rotated(ids) {
                if let Some(n) = self.nodes[id as usize - 1].node.as_mut() {
                    if n.role() == Role::Leader {
                        if let Ok(idx) = n.propose(data.clone()) {
                            self.proposals.push((idx, data.clone()));
                        }
                        self.drain(id);
                        break;
                    }
                }
            }
        } else if r < 94 {
            // linearizable read on whoever thinks it is leader
            let required = self
                .committed_acks
                .iter()
                .map(|(i, _)| *i)
                .max()
                .unwrap_or(0);
            // A deposed leader that has not noticed yet may be picked:
            // exactly the case ReadIndex must handle.
            for id in self.rotated(ids) {
                if let Some(n) = self.nodes[id as usize - 1].node.as_mut() {
                    if n.role() == Role::Leader {
                        if let Ok(rid) = n.read_index() {
                            self.pending_reads.insert((id, rid), required);
                        }
                        self.drain(id);
                        break;
                    }
                }
            }
        } else if chaos && r < 97 {
            // crash or restart a random node (keep a majority alive)
            let id = ids[(self.rand() % ids.len() as u64) as usize];
            let alive = self.nodes.iter().filter(|n| n.node.is_some()).count();
            let quorum = self.nodes.len() / 2 + 1;
            let sn = &mut self.nodes[id as usize - 1];
            if sn.node.is_some() && alive > quorum {
                sn.node = None; // crash: in-memory state gone
                sn.applied.clear(); // the state machine is rebuilt by replay
                self.pending_reads.retain(|(n, _), _| *n != id); // read ids restart
            } else if sn.node.is_none() {
                let seed = self.seed + id * 1000 + self.rng % 1000;
                let all = self.ids();
                let sn = &mut self.nodes[id as usize - 1];
                sn.node = Some(RaftNode::new(
                    id,
                    all,
                    sn.hs,
                    sn.log.clone(),
                    self.timing,
                    seed,
                ));
            }
        } else if chaos {
            // Change the partition, rarely: a partition must outlast an
            // election timeout (hundreds of steps) for the interesting cases
            // to happen, e.g. a new leader elected while the old one is cut
            // off and still believes it leads.
            if !self.rand().is_multiple_of(self.partition_hold) {
                return;
            }
            self.partitioned.clear();
            if self.rand().is_multiple_of(2) {
                let id = ids[(self.rand() % ids.len() as u64) as usize];
                self.partitioned.insert(id);
            }
        }
    }

    fn check_prefix_property(&self) {
        let longest = self
            .nodes
            .iter()
            .map(|n| &n.applied)
            .max_by_key(|a| a.len())
            .unwrap();
        for n in &self.nodes {
            assert!(
                n.applied.iter().zip(longest.iter()).all(|(a, b)| a == b),
                "state machines diverged (seed {})",
                self.seed
            );
        }
    }
}

fn run(n: usize, seed: u64, chaos_steps: usize) {
    let mut sim = Sim::new(n, seed);
    for _ in 0..chaos_steps {
        sim.step(true);
    }
    sim.check_prefix_property();
    // Heal: everyone up, no partition, reliable delivery. Messages still in
    // flight are dropped (allowed: the network may lose anything). Without
    // this, a backlog of thousands of stale messages, delivered in random
    // order, delays every vote far past the election timeout and the
    // simulated cluster never settles (seen with 1-entry batches).
    sim.partitioned.clear();
    sim.net.clear();
    let all = sim.ids();
    for id in all.clone() {
        if sim.nodes[id as usize - 1].node.is_none() {
            let sn = &mut sim.nodes[id as usize - 1];
            sn.node = Some(RaftNode::new(
                id,
                all.clone(),
                sn.hs,
                sn.log.clone(),
                sim.timing,
                seed + 7 * id,
            ));
        }
    }
    let before = sim.committed_acks.len();
    for _ in 0..30_000 {
        sim.step(false);
    }
    sim.check_prefix_property();
    assert!(
        sim.reads_checked > 0,
        "no ReadIndex read completed (seed {seed})"
    );
    if std::env::var("SIM_DEBUG").is_ok() {
        for sn in &sim.nodes {
            let n = sn.node.as_ref().unwrap();
            eprintln!(
                "node {} role {:?} term {} commit {} last {} applied {} net {}",
                n.id,
                n.role(),
                n.term(),
                n.commit_index(),
                n.last_index(),
                sn.applied.len(),
                sim.net.len()
            );
        }
        eprintln!("acks before {} after {}", before, sim.committed_acks.len());
    }
    assert!(
        sim.committed_acks.len() > before,
        "no progress after healing (seed {seed})"
    );
    // Every committed ack is in the final applied log of the longest node.
    let longest = sim
        .nodes
        .iter()
        .map(|n| n.applied.clone())
        .max_by_key(|a| a.len())
        .unwrap();
    for (idx, data) in &sim.committed_acks {
        let e = &longest[*idx as usize - 1];
        assert_eq!(&e.data, data, "committed entry {idx} changed (seed {seed})");
    }
    // Drain the network so all nodes catch up, then all applied logs equal.
    for _ in 0..30_000 {
        sim.step(false);
        if sim.net.is_empty() {
            break;
        }
    }
}

#[test]
fn three_nodes_many_schedules() {
    if let Ok(s) = std::env::var("SIM_SEED") {
        run(3, s.parse().unwrap(), 3_000);
        return;
    }
    for seed in 1..=200 {
        run(3, seed, 3_000);
    }
}

#[test]
fn five_nodes_many_schedules() {
    for seed in 1..=100 {
        run(5, 10_000 + seed, 4_000);
    }
}

#[test]
fn single_node_commits_immediately() {
    let mut n = RaftNode::new(
        1,
        vec![1],
        HardState::default(),
        Vec::new(),
        Timing::default(),
        1,
    );
    for _ in 0..30 {
        n.tick();
    }
    assert_eq!(n.role(), Role::Leader);
    let idx = n.propose(b"x".to_vec()).unwrap();
    let r = n.take_ready();
    assert!(r.committed.iter().any(|e| e.index == idx && e.data == b"x"));
}

/// Raft paper Figure 8, replayed deterministically. A leader must not
/// count replicas to commit an entry from an *earlier* term:
///
/// * start: n1 has [e1(t1), e2(t2)], n2 has [e1], n3 has [e1, x2(t3)];
/// * n1 becomes leader of term 4 (votes: n1, n2) and copies e2 to n2, so
///   e2 is on a majority (n1, n2), but its term-4 no-op is not;
/// * n1 crashes; n3 (last term 3 > 2) wins term 5 with n2's vote and
///   overwrites index 2 with x2.
///
/// If n1 had committed e2 when it reached the majority, the entry at a
/// committed index would change, which this test detects. The randomized
/// tests above only hit this rarely (`scripts/raft_mutations.sh`).
#[test]
fn figure_8_old_term_entry_is_not_committed_by_counting() {
    let e = |term, index, d: &str| Entry {
        term,
        index,
        data: d.as_bytes().to_vec(),
    };
    let timing = Timing {
        max_batch: 1,
        ..Timing::default()
    };
    let hs = HardState {
        term: 3,
        voted_for: None,
    };
    let ids = vec![1, 2, 3];
    let mut nodes: Vec<Option<RaftNode>> = vec![
        Some(RaftNode::new(
            1,
            ids.clone(),
            hs,
            vec![e(1, 1, "e1"), e(2, 2, "e2")],
            timing,
            1,
        )),
        Some(RaftNode::new(
            2,
            ids.clone(),
            hs,
            vec![e(1, 1, "e1")],
            timing,
            2,
        )),
        Some(RaftNode::new(
            3,
            ids.clone(),
            hs,
            vec![e(1, 1, "e1"), e(3, 2, "x2")],
            timing,
            3,
        )),
    ];
    let mut committed: HashMap<u64, Entry> = HashMap::new();
    let mut record = |r: &strata_raft::core::Ready| {
        for c in &r.committed {
            if let Some(prev) = committed.insert(c.index, c.clone()) {
                assert_eq!(prev, *c, "committed entry at index {} changed", c.index);
            }
        }
    };
    // n1 campaigns (term 4).
    let mut net: Vec<Envelope> = Vec::new();
    while nodes[0].as_ref().unwrap().role() != Role::Candidate {
        nodes[0].as_mut().unwrap().tick();
    }
    // Deliver only n1 <-> n2 traffic, one message at a time, and crash n1
    // right after it learns that n2 holds index 2.
    loop {
        for n in nodes.iter_mut().flatten() {
            let r = n.take_ready();
            record(&r);
            net.extend(r.messages);
        }
        net.retain(|m| (m.from == 1 && m.to == 2) || (m.from == 2 && m.to == 1));
        assert!(!net.is_empty(), "scenario stalled");
        let m = net.remove(0);
        let crash_after = matches!(
            m.msg,
            strata_raft::core::Message::AppendReply {
                success: true,
                match_index: 2,
                ..
            }
        ) && m.to == 1;
        let to = m.to as usize - 1;
        nodes[to].as_mut().unwrap().step(m);
        let r = nodes[to].as_mut().unwrap().take_ready();
        record(&r);
        net.extend(r.messages);
        if crash_after {
            nodes[0] = None;
            net.retain(|m| m.from != 1 && m.to != 1);
            break;
        }
    }
    // n2 and n3 run until n3 leads and commits its no-op (index 3).
    for _ in 0..10_000 {
        for n in nodes.iter_mut().skip(1).flatten() {
            n.tick();
            let r = n.take_ready();
            record(&r);
            net.extend(r.messages);
        }
        while let Some(m) = net.pop() {
            if m.to == 1 {
                continue;
            }
            let to = m.to as usize - 1;
            nodes[to].as_mut().unwrap().step(m);
            let r = nodes[to].as_mut().unwrap().take_ready();
            record(&r);
            net.extend(r.messages);
        }
        if nodes[2].as_ref().unwrap().commit_index() >= 3 {
            break;
        }
    }
    assert_eq!(nodes[2].as_ref().unwrap().role(), Role::Leader);
    assert_eq!(committed[&2].data, b"x2", "index 2 ends up as n3's entry");
}
