//! The Raft consensus algorithm (Ongaro & Ousterhout, 2014) as a pure,
//! deterministic state machine: no threads, no clocks, no I/O.
//!
//! The caller drives it:
//! * [`RaftNode::tick`] advances logical time (elections, heartbeats);
//! * [`RaftNode::step`] delivers a message from a peer;
//! * [`RaftNode::propose`] appends a client command (leader only);
//! * [`RaftNode::take_ready`] returns what the caller must do next, **in
//!   order**: persist the hard state and log changes (fsync), then send the
//!   messages, then apply the newly committed entries to the state machine.
//!
//! Persist-before-send is what makes votes and acknowledgements safe across
//! crashes: a node never tells anyone "I voted for you" or "I have entry i"
//! before that fact is on disk.
//!
//! Keeping the core free of I/O lets the tests run thousands of randomized
//! schedules (message loss, reordering, partitions, crashes) and check the
//! Raft safety properties directly (see `tests`).
//!
//! Implemented: leader election with randomized timeouts and the up-to-date
//! log check, log replication with the consistency check and
//! conflict back-off, commitment only of current-term entries (Figure 8),
//! ReadIndex for linearizable reads. Not implemented: log compaction /
//! snapshots, membership changes, pre-vote, leadership transfer.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

pub type NodeId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub term: u64,
    pub index: u64,
    pub data: Vec<u8>,
}

/// State that must survive a crash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardState {
    pub term: u64,
    pub voted_for: Option<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    RequestVote {
        term: u64,
        last_index: u64,
        last_term: u64,
    },
    Vote {
        term: u64,
        granted: bool,
    },
    Append {
        term: u64,
        prev_index: u64,
        prev_term: u64,
        entries: Vec<Entry>,
        commit: u64,
        read_ctx: u64,
    },
    /// `match_index` on success; on failure a hint where to retry.
    AppendReply {
        term: u64,
        success: bool,
        match_index: u64,
        hint: u64,
        read_ctx: u64,
    },
}

impl Message {
    pub fn term(&self) -> u64 {
        match self {
            Message::RequestVote { term, .. }
            | Message::Vote { term, .. }
            | Message::Append { term, .. }
            | Message::AppendReply { term, .. } => *term,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub from: NodeId,
    pub to: NodeId,
    pub msg: Message,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Follower,
    Candidate,
    Leader,
}

/// Everything the caller must do after a call into the node.
#[derive(Debug, Default)]
pub struct Ready {
    /// Persist this hard state (if `Some`) *before* sending messages.
    pub hard_state: Option<HardState>,
    /// Remove log entries with index >= this (conflict), before appending.
    pub truncate_from: Option<u64>,
    /// Append these entries to the durable log, before sending messages.
    pub append: Vec<Entry>,
    /// Then send these.
    pub messages: Vec<Envelope>,
    /// Then apply these (in order) to the state machine.
    pub committed: Vec<Entry>,
    /// Read requests (by id) that are now safe to serve once the state
    /// machine has applied up to the given index.
    pub reads: Vec<(u64, u64)>,
}

/// Timing, in ticks.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub heartbeat: u64,
    /// Election timeout is drawn uniformly from [min, max).
    pub election_min: u64,
    pub election_max: u64,
    /// Most entries per AppendEntries message (the simulation tests also
    /// use tiny values, so replicating a long tail takes several messages).
    pub max_batch: usize,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            heartbeat: 1,
            election_min: 10,
            election_max: 20,
            max_batch: 64,
        }
    }
}

struct Progress {
    next: u64,
    matched: u64,
}

pub struct RaftNode {
    pub id: NodeId,
    peers: Vec<NodeId>,
    hs: HardState,
    role: Role,
    leader: Option<NodeId>,
    /// In-memory copy of the durable log; `log[i]` has index `i + 1`.
    log: Vec<Entry>,
    commit: u64,
    applied: u64,
    votes: HashMap<NodeId, bool>,
    progress: HashMap<NodeId, Progress>,
    timing: Timing,
    elapsed: u64,
    timeout: u64,
    rng: u64,
    ready: Ready,
    // ReadIndex bookkeeping (leader): read id -> (commit index at request, acks)
    next_read: u64,
    pending_reads: BTreeMap<u64, (u64, Vec<NodeId>)>,
}

impl RaftNode {
    /// Create a node from its durable state (empty for a new node).
    pub fn new(
        id: NodeId,
        peers: Vec<NodeId>,
        hs: HardState,
        log: Vec<Entry>,
        timing: Timing,
        seed: u64,
    ) -> RaftNode {
        for (i, e) in log.iter().enumerate() {
            assert_eq!(e.index, i as u64 + 1, "log must be dense from 1");
        }
        let mut n = RaftNode {
            id,
            peers: peers.into_iter().filter(|&p| p != id).collect(),
            hs,
            role: Role::Follower,
            leader: None,
            log,
            commit: 0,
            applied: 0,
            votes: HashMap::new(),
            progress: HashMap::new(),
            timing,
            elapsed: 0,
            timeout: 0,
            rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            ready: Ready::default(),
            next_read: 1,
            pending_reads: BTreeMap::new(),
        };
        n.reset_timeout();
        n
    }

    pub fn role(&self) -> Role {
        self.role
    }
    pub fn term(&self) -> u64 {
        self.hs.term
    }
    pub fn leader(&self) -> Option<NodeId> {
        self.leader
    }
    pub fn commit_index(&self) -> u64 {
        self.commit
    }
    pub fn last_index(&self) -> u64 {
        self.log.len() as u64
    }
    pub fn log(&self) -> &[Entry] {
        &self.log
    }

    fn term_at(&self, index: u64) -> u64 {
        if index == 0 {
            0
        } else {
            self.log.get(index as usize - 1).map_or(0, |e| e.term)
        }
    }

    fn last_term(&self) -> u64 {
        self.term_at(self.last_index())
    }

    fn quorum(&self) -> usize {
        let members = self.peers.len() + 1;
        members / 2 + 1 // a strict majority
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn reset_timeout(&mut self) {
        self.elapsed = 0;
        let span = self.timing.election_max - self.timing.election_min;
        self.timeout = self.timing.election_min + self.rand() % span.max(1);
    }

    fn send(&mut self, to: NodeId, msg: Message) {
        self.ready.messages.push(Envelope {
            from: self.id,
            to,
            msg,
        });
    }

    fn set_hard_state(&mut self, hs: HardState) {
        if hs != self.hs {
            self.hs = hs;
            self.ready.hard_state = Some(hs);
        }
    }

    fn become_follower(&mut self, term: u64, leader: Option<NodeId>) {
        if term > self.hs.term {
            self.set_hard_state(HardState {
                term,
                voted_for: None,
            });
        }
        self.role = Role::Follower;
        self.leader = leader;
        self.pending_reads.clear();
        self.reset_timeout();
    }

    /// Advance logical time by one tick.
    pub fn tick(&mut self) {
        self.elapsed += 1;
        match self.role {
            Role::Leader => {
                if self.elapsed >= self.timing.heartbeat {
                    self.elapsed = 0;
                    self.broadcast_append(0);
                }
            }
            _ => {
                if self.elapsed >= self.timeout {
                    self.campaign();
                }
            }
        }
    }

    fn campaign(&mut self) {
        let term = self.hs.term + 1;
        self.set_hard_state(HardState {
            term,
            voted_for: Some(self.id),
        });
        self.role = Role::Candidate;
        self.leader = None;
        self.votes.clear();
        self.votes.insert(self.id, true);
        self.reset_timeout();
        if self.votes.len() >= self.quorum() {
            self.become_leader(); // single-node cluster
            return;
        }
        let (last_index, last_term) = (self.last_index(), self.last_term());
        for p in self.peers.clone() {
            self.send(
                p,
                Message::RequestVote {
                    term,
                    last_index,
                    last_term,
                },
            );
        }
    }

    fn become_leader(&mut self) {
        self.role = Role::Leader;
        self.leader = Some(self.id);
        self.elapsed = 0;
        let next = self.last_index() + 1;
        self.progress = self
            .peers
            .iter()
            .map(|&p| (p, Progress { next, matched: 0 }))
            .collect();
        // A no-op entry of the new term lets the leader commit entries from
        // earlier terms (it may only count replicas for current-term entries).
        let term = self.hs.term;
        self.append_local(vec![Vec::new()], term);
        self.broadcast_append(0);
    }

    fn append_local(&mut self, datas: Vec<Vec<u8>>, term: u64) -> u64 {
        for data in datas {
            let e = Entry {
                term,
                index: self.last_index() + 1,
                data,
            };
            self.ready.append.push(e.clone());
            self.log.push(e);
        }
        self.maybe_commit();
        self.last_index()
    }

    /// Propose a command. Returns its log index, or `Err(leader hint)`.
    pub fn propose(&mut self, data: Vec<u8>) -> Result<u64, Option<NodeId>> {
        if self.role != Role::Leader {
            return Err(self.leader);
        }
        let term = self.hs.term;
        let idx = self.append_local(vec![data], term);
        self.broadcast_append(0);
        Ok(idx)
    }

    /// Start a linearizable read (ReadIndex). The returned read id appears
    /// in `Ready::reads` with the index the state machine must reach first,
    /// once a quorum has confirmed this node is still the leader.
    pub fn read_index(&mut self) -> Result<u64, Option<NodeId>> {
        if self.role != Role::Leader {
            return Err(self.leader);
        }
        // Only valid once an entry of the current term is committed (the
        // leader then knows the true commit index).
        let id = self.next_read;
        self.next_read += 1;
        self.pending_reads.insert(id, (self.commit, vec![self.id]));
        self.broadcast_append(id);
        self.check_reads();
        Ok(id)
    }

    fn check_reads(&mut self) {
        if self.term_at(self.commit) != self.hs.term {
            return; // no current-term commit yet
        }
        let quorum = self.quorum();
        let done: Vec<u64> = self
            .pending_reads
            .iter()
            .filter(|(_, (_, acks))| acks.len() >= quorum)
            .map(|(&id, _)| id)
            .collect();
        for id in done {
            let (idx, _) = self.pending_reads.remove(&id).unwrap();
            // The index recorded at request time may predate this term's
            // first commit (a new leader does not yet know the true commit
            // index), so wait for at least the current commit index: it is
            // committed and >= everything acknowledged before the read began.
            self.ready.reads.push((id, idx.max(self.commit)));
        }
    }

    fn broadcast_append(&mut self, read_ctx: u64) {
        for p in self.peers.clone() {
            self.send_append(p, read_ctx);
        }
    }

    fn send_append(&mut self, to: NodeId, read_ctx: u64) {
        let next = self.progress[&to].next;
        let prev_index = next - 1;
        let prev_term = self.term_at(prev_index);
        // Cap batch size so one message stays small.
        let entries: Vec<Entry> = self
            .log
            .iter()
            .skip(prev_index as usize)
            .take(self.timing.max_batch)
            .cloned()
            .collect();
        let msg = Message::Append {
            term: self.hs.term,
            prev_index,
            prev_term,
            entries,
            commit: self.commit,
            read_ctx,
        };
        self.send(to, msg);
    }

    /// Deliver a message from a peer.
    pub fn step(&mut self, env: Envelope) {
        let from = env.from;
        let term = env.msg.term();
        if term > self.hs.term {
            // Any message from a newer term makes us a follower of that term.
            let leader = matches!(env.msg, Message::Append { .. }).then_some(from);
            self.become_follower(term, leader);
        }
        match env.msg {
            Message::RequestVote {
                term,
                last_index,
                last_term,
            } => {
                let up_to_date = (last_term, last_index) >= (self.last_term(), self.last_index());
                let can_vote = self.hs.voted_for.is_none_or(|v| v == from);
                let granted = term == self.hs.term && can_vote && up_to_date;
                if granted {
                    self.set_hard_state(HardState {
                        term,
                        voted_for: Some(from),
                    });
                    self.reset_timeout();
                }
                self.send(
                    from,
                    Message::Vote {
                        term: self.hs.term,
                        granted,
                    },
                );
            }
            Message::Vote { term, granted } => {
                if self.role == Role::Candidate && term == self.hs.term {
                    self.votes.insert(from, granted);
                    if self.votes.values().filter(|&&g| g).count() >= self.quorum() {
                        self.become_leader();
                    }
                }
            }
            Message::Append {
                term,
                prev_index,
                prev_term,
                entries,
                commit,
                read_ctx,
            } => {
                if term < self.hs.term {
                    self.send(
                        from,
                        Message::AppendReply {
                            term: self.hs.term,
                            success: false,
                            match_index: 0,
                            hint: 0,
                            read_ctx,
                        },
                    );
                    return;
                }
                // Valid leader for this term.
                self.role = Role::Follower;
                self.leader = Some(from);
                self.reset_timeout();
                if prev_index > self.last_index() || self.term_at(prev_index) != prev_term {
                    // Consistency check failed. Hint where the leader should
                    // retry: just past our log if it is too short, otherwise
                    // the first index of the conflicting term (skipping the
                    // whole term in one round trip), never below commit + 1.
                    let hint = if prev_index > self.last_index() {
                        self.last_index() + 1
                    } else {
                        let bad = self.term_at(prev_index);
                        let mut i = prev_index;
                        while i > self.commit + 1 && self.term_at(i - 1) == bad {
                            i -= 1;
                        }
                        i
                    };
                    self.send(
                        from,
                        Message::AppendReply {
                            term,
                            success: false,
                            match_index: 0,
                            hint,
                            read_ctx,
                        },
                    );
                    return;
                }
                // Append, truncating any conflicting suffix.
                let last_new = prev_index + entries.len() as u64;
                for e in entries {
                    let i = e.index;
                    if i <= self.last_index() {
                        if self.term_at(i) == e.term {
                            continue; // already have it
                        }
                        assert!(i > self.commit, "never truncate committed entries");
                        self.log.truncate(i as usize - 1);
                        self.ready.append.retain(|x| x.index < i);
                        self.ready.truncate_from =
                            Some(self.ready.truncate_from.map_or(i, |t| t.min(i)));
                    }
                    self.log.push(e.clone());
                    self.ready.append.push(e);
                }
                // We now match the leader up to `last_new` (our log may be
                // longer with entries the leader will overwrite or confirm).
                let match_index = last_new;
                // Commit what the leader committed, but only entries we know
                // match the leader's log (<= last_new).
                let new_commit = commit.min(last_new);
                if new_commit > self.commit {
                    self.commit = new_commit;
                    self.emit_committed();
                }
                self.send(
                    from,
                    Message::AppendReply {
                        term,
                        success: true,
                        match_index,
                        hint: 0,
                        read_ctx,
                    },
                );
            }
            Message::AppendReply {
                term,
                success,
                match_index,
                hint,
                read_ctx,
            } => {
                if self.role != Role::Leader || term != self.hs.term {
                    return;
                }
                if read_ctx > 0 {
                    if let Some((_, acks)) = self.pending_reads.get_mut(&read_ctx) {
                        if !acks.contains(&from) {
                            acks.push(from);
                        }
                    }
                    // Heartbeat acks for older read ids also confirm leadership
                    // at least as of then; newer requests need their own round.
                    self.check_reads();
                }
                let Some(p) = self.progress.get_mut(&from) else {
                    return;
                };
                if success {
                    // Only a reply that moves `matched` forward continues the
                    // catch-up pipeline. Every heartbeat to a lagging follower
                    // also gets a success reply; if each of those triggered a
                    // follow-up, the number of in-flight catch-up chains would
                    // grow with every heartbeat (seen in the simulation with
                    // 1-entry batches: >10,000 messages in flight).
                    let advanced = match_index > p.matched;
                    if advanced {
                        p.matched = match_index;
                    }
                    p.next = p.next.max(p.matched + 1);
                    self.maybe_commit();
                    if advanced && self.progress[&from].next <= self.last_index() {
                        self.send_append(from, 0); // more to send
                    }
                } else {
                    // Back off; the hint lets us skip a whole conflicting term.
                    // Never below matched + 1: entries the follower has
                    // acknowledged in this term are durable and match ours,
                    // so a rejection below that is a stale (old) reply.
                    let next = hint
                        .clamp(1, p.next.saturating_sub(1).max(1))
                        .max(p.matched + 1);
                    if next == p.next {
                        return; // stale rejection; the next heartbeat retries anyway
                    }
                    p.next = next;
                    self.send_append(from, 0);
                }
            }
        }
    }

    /// Leader: commit the highest index replicated on a quorum, *if* that
    /// entry is from the current term (Raft paper, section 5.4.2 / Figure 8).
    fn maybe_commit(&mut self) {
        if self.role != Role::Leader {
            return;
        }
        let mut matched: Vec<u64> = self.progress.values().map(|p| p.matched).collect();
        matched.push(self.last_index());
        matched.sort_unstable_by(|a, b| b.cmp(a));
        let candidate = matched[self.quorum() - 1];
        if candidate > self.commit && self.term_at(candidate) == self.hs.term {
            self.commit = candidate;
            self.emit_committed();
            self.check_reads();
        }
    }

    fn emit_committed(&mut self) {
        while self.applied < self.commit {
            self.applied += 1;
            self.ready
                .committed
                .push(self.log[self.applied as usize - 1].clone());
        }
    }

    /// Take the pending work (see [`Ready`]).
    pub fn take_ready(&mut self) -> Ready {
        std::mem::take(&mut self.ready)
    }
}
