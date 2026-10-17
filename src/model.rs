//! Stored records and the index entries derived from them.
//!
//! Records are the stored form, not the API shape: views are assembled in
//! `domain` from these records plus live Git state. Every derived index entry
//! of a record comes from one function here, used both when the record is
//! written and when `branvil admin check` / `reindex` rebuilds the indexes, so
//! the two cannot disagree.

use redb::TableHandle;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::store::{self, Key, Rd, Tbl, Wtx};
use crate::util::fold;

/// What a member may do follows from its kind alone.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    /// The one person who uses Branvil, created by `branvil init`: may do
    /// everything.
    Owner,
    /// A GuildBotics AI member: reads everything and works on tickets, pull
    /// requests, and unprotected branches of every repository.
    Agent,
    /// A CI runner: uses the runner API only.
    Runner,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Member {
    pub name: String,
    pub display_name: String,
    pub kind: MemberKind,
    /// Accounts on upstream code hosts, one per provider. Their credentials
    /// are files under `<data>/secrets/accounts/`, never in this record.
    pub accounts: Vec<Account>,
    pub created_at: String,
    /// The version of the member's picture, whose bytes are in `AVATARS`
    /// under the member's key; `None` for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// A member's account on an upstream code host.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Account {
    /// The provider (`github`).
    pub provider: String,
    /// The login the host reported for the credential when it was set.
    pub login: String,
    pub kind: AccountKind,
    pub set_at: String,
}

/// What an account's credential is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// A token of the account itself.
    Token,
    /// An app's identity, of which the host makes short-lived tokens.
    App,
}

impl Member {
    pub fn key(name: &str) -> Vec<u8> {
        Key::new().s(&fold(name)).bytes()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Token {
    pub id: String,
    pub member: String,
    pub label: String,
    /// Hex SHA-256 of the token's secret part. The token itself is never stored.
    pub hash: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl Token {
    pub fn key(id: &str) -> Vec<u8> {
        Key::new().s(id).bytes()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Session {
    pub member: String,
    pub csrf: String,
    pub created_at: String,
    pub expires_at: String,
}

/// A workspace: the repositories under it and its one board.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

impl Workspace {
    pub fn key(id: &str) -> Vec<u8> {
        Key::new().s(id).bytes()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Repo {
    pub id: String,
    /// `<workspace name>/<name>`.
    pub path: String,
    /// The id of the workspace it is in.
    pub workspace: String,
    pub default_branch: String,
    /// Configured protected branches; the default branch is protected in
    /// addition to these.
    pub protected_branches: Vec<String>,
    pub required_checks: Vec<String>,
    pub ci_network: bool,
    /// The labels its tickets may carry.
    pub labels: Vec<String>,
    /// The code host its default branch follows and its pull requests are
    /// published to.
    pub upstream: Option<Upstream>,
    pub revision: u64,
    pub created_at: String,
}

/// Where a repository's work goes: a repository on an upstream code host.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Upstream {
    /// The provider (`github`).
    pub provider: String,
    /// The repository there, as the provider names it (`owner/name`).
    pub repository: String,
    /// The member whose account reads the upstream.
    pub reader: Reader,
}

impl Upstream {
    /// The same repository on the same host (whoever reads it).
    pub fn same_place(&self, provider: &str, repository: &str) -> bool {
        self.provider == provider && self.repository == repository
    }
}

/// Who reads an upstream. Chosen once and kept: only the owner's choice, or
/// removing that member's account, changes it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Reader {
    pub member: String,
    /// Chosen by the owner, as opposed to picked when nobody was named.
    pub chosen: bool,
}

impl Repo {
    pub fn key(id: &str) -> Vec<u8> {
        Key::new().s(id).bytes()
    }

    pub fn protected(&self) -> Vec<String> {
        let mut all = vec![self.default_branch.clone()];
        for b in &self.protected_branches {
            if !all.contains(b) {
                all.push(b.clone());
            }
        }
        all
    }

    pub fn is_protected(&self, branch: &str) -> bool {
        self.protected().iter().any(|b| b == branch)
    }
}

pub const LANES: [&str; 4] = ["new", "ready", "in_progress", "done"];

pub fn lane_index(lane: &str) -> Option<u64> {
    LANES.iter().position(|l| *l == lane).map(|i| i as u64)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Assignee {
    pub name: String,
    pub assigned_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ticket {
    /// The id of the repository the ticket belongs to.
    pub repo: String,
    /// The id of that repository's workspace, whose board the ticket may be on.
    pub workspace: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub author: String,
    pub assignee: Option<Assignee>,
    pub labels: Vec<String>,
    /// The issue it stands for on its repository's upstream host.
    pub upstream_issue: Option<String>,
    /// The lane and rank on the workspace's board; `None` when the ticket is
    /// not on the board.
    pub lane: Option<String>,
    pub rank: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub revision: u64,
}

impl Ticket {
    pub fn key(repo: &str, number: u64) -> Vec<u8> {
        Key::new().s(repo).u(number).bytes()
    }

    pub fn card_key(workspace: &str, lane: &str, rank: &str, repo: &str, number: u64) -> Vec<u8> {
        Key::new()
            .s(workspace)
            .u(lane_index(lane).unwrap_or(u64::MAX))
            .s(rank)
            .s(repo)
            .u(number)
            .bytes()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Reaction {
    pub id: String,
    pub author: String,
    pub reaction: String,
}

/// A conversation comment of a ticket or a pull request.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Comment {
    pub id: String,
    pub seq: u64,
    pub author: String,
    pub body: String,
    pub meta: Option<serde_json::Value>,
    pub created_at: String,
    pub reactions: Vec<Reaction>,
}

/// What a comment belongs to.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Ticket { repo: String, number: u64 },
    Pull { repo: String, number: u64 },
}

impl Target {
    pub fn prefix(&self) -> Vec<u8> {
        match self {
            Target::Ticket { repo, number } => Key::new().s("ticket").s(repo).u(*number),
            Target::Pull { repo, number } => Key::new().s("pull").s(repo).u(*number),
        }
        .bytes()
    }

    pub fn comment_key(&self, seq: u64) -> Vec<u8> {
        let mut key = self.prefix();
        key.extend(Key::new().u(seq).bytes());
        key
    }

    /// The owner string kept with an entry id, compared when an id is used.
    pub fn owner(&self) -> String {
        match self {
            Target::Ticket { repo, number } => format!("ticket:{repo}:{number}"),
            Target::Pull { repo, number } => format!("pull:{repo}:{number}"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TicketLink {
    pub url: String,
    pub closes: bool,
    /// The (repository id, number) of a ticket of this Branvil.
    pub local: Option<(String, u64)>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Pull {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub merged: bool,
    pub draft: bool,
    pub author: String,
    pub head: String,
    pub base: String,
    pub reviewers: Vec<String>,
    pub tickets: Vec<TicketLink>,
    pub labels: Vec<String>,
    /// Members who reviewed; with the author and reviewers they make up who the
    /// pull request involves.
    pub reviewed_by: Vec<String>,
    pub ready_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub merge_sha: Option<String>,
    /// The base tip the merge moved from (so the merged diff stays available).
    pub merged_base: Option<String>,
    /// Set once the pull request was published to the upstream host.
    pub upstream: Option<Published>,
    pub revision: u64,
}

/// A pull request published to the upstream host, and what Branvil last sent
/// to it (`docs/architecture.md`, "Upstream").
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Published {
    /// Where it was published: the provider and the repository there. It stays
    /// tied to them when the repository's upstream changes.
    pub provider: String,
    pub repository: String,
    /// The member whose account writes to the host (the author).
    pub account: String,
    /// The host's pull request; `None` until it was created.
    pub number: Option<u64>,
    pub url: Option<String>,
    /// The host's state as last read: `none` (no pull request there yet),
    /// `open`, `closed`, or `merged`.
    pub state: String,
    /// The commit Branvil last pushed to the host's branch.
    pub pushed: Option<String>,
    pub sent_title: String,
    pub sent_body: String,
    /// `open` or `closed`, as last sent or read.
    pub sent_state: String,
    /// The host's check runs of `pushed`.
    pub checks: Vec<HostCheck>,
    pub published_at: String,
    /// The last failure; the next poll retries.
    pub error: Option<String>,
    /// Why sending the branch stopped (the host has commits Branvil did not
    /// push, or refused); only publishing again resumes it.
    pub stopped: Option<String>,
}

/// One check run on the upstream host.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HostCheck {
    pub id: String,
    pub name: String,
    /// `queued`, `in_progress`, or `completed`.
    pub status: String,
    pub conclusion: Option<String>,
    pub url: Option<String>,
    pub summary: Option<String>,
}

impl Pull {
    pub fn key(repo: &str, number: u64) -> Vec<u8> {
        Key::new().s(repo).u(number).bytes()
    }

    pub fn open_key(repo: &str, head: &str, base: &str) -> Vec<u8> {
        Key::new().s(repo).s(head).s(base).bytes()
    }

    pub fn involved(&self) -> Vec<String> {
        let mut all: Vec<String> = Vec::new();
        for m in std::iter::once(&self.author)
            .chain(&self.reviewers)
            .chain(&self.reviewed_by)
        {
            let m = fold(m);
            if !all.contains(&m) {
                all.push(m);
            }
        }
        all
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Review {
    pub id: String,
    pub seq: u64,
    pub author: String,
    pub verdict: String,
    pub body: String,
    pub commit: String,
    pub submitted_at: String,
    pub reactions: Vec<Reaction>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Thread {
    pub id: String,
    pub root_seq: u64,
    pub path: String,
    pub line: u64,
    pub side: String,
    pub start_line: Option<u64>,
    pub start_side: Option<String>,
    pub commit: String,
    pub resolved: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InlineComment {
    pub id: String,
    pub seq: u64,
    pub root_seq: u64,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub reactions: Vec<Reaction>,
    pub reply_to_id: Option<String>,
}

pub fn review_key(repo: &str, number: u64, seq: u64) -> Vec<u8> {
    Key::new().s(repo).u(number).u(seq).bytes()
}

pub fn thread_key(repo: &str, number: u64, root_seq: u64) -> Vec<u8> {
    Key::new().s(repo).u(number).u(root_seq).bytes()
}

pub fn inline_key(repo: &str, number: u64, root_seq: u64, seq: u64) -> Vec<u8> {
    Key::new().s(repo).u(number).u(root_seq).u(seq).bytes()
}

/// Where an entry id points: the table, the record key, and the item that owns
/// it (`Target::owner`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EntryRef {
    pub kind: String,
    pub owner: String,
    pub key: String,
}

/// A ref change that was started and not yet confirmed. Written before
/// `git update-ref`, removed after the database reflects the change.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct JournalEntry {
    pub id: String,
    pub repo: String,
    pub refname: String,
    pub old: String,
    pub new: String,
    pub op: JournalOp,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalOp {
    Merge { number: u64, merged_by: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IdempotencyRecord {
    pub body_hash: String,
    pub status: u16,
    pub body: String,
    pub created_at: String,
}

/// One derived index entry.
pub struct IxEntry {
    pub table: Tbl,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

impl IxEntry {
    fn mark(table: Tbl, key: Vec<u8>) -> Self {
        Self {
            table,
            key,
            value: Vec::new(),
        }
    }

    fn same(&self, other: &IxEntry) -> bool {
        self.table.name() == other.table.name()
            && self.key == other.key
            && self.value == other.value
    }
}

pub fn path_key(kind: &str, path: &str) -> Vec<u8> {
    Key::new().s(kind).s(&fold(path)).bytes()
}

pub fn token_index(t: &Token) -> Vec<IxEntry> {
    vec![IxEntry::mark(
        store::IX_MEMBER_TOKENS,
        Key::new().s(&t.member).s(&t.id).bytes(),
    )]
}

pub fn repo_index(r: &Repo) -> Vec<IxEntry> {
    vec![IxEntry {
        table: store::IX_PATHS,
        key: path_key("repo", &r.path),
        value: r.id.as_bytes().to_vec(),
    }]
}

pub fn workspace_index(x: &Workspace) -> Vec<IxEntry> {
    vec![IxEntry {
        table: store::IX_PATHS,
        key: path_key("workspace", &x.name),
        value: x.id.as_bytes().to_vec(),
    }]
}

pub fn ticket_index(t: &Ticket) -> Vec<IxEntry> {
    let mut out = Vec::new();
    if let (Some(lane), Some(rank)) = (&t.lane, &t.rank) {
        out.push(IxEntry::mark(
            store::IX_CARDS,
            Ticket::card_key(&t.workspace, lane, rank, &t.repo, t.number),
        ));
    }
    for label in &t.labels {
        out.push(IxEntry::mark(
            store::IX_TICKET_LABELS,
            Key::new().s(&t.repo).s(label).u(t.number).bytes(),
        ));
    }
    if let (Some(closed_at), "closed") = (&t.closed_at, t.state.as_str()) {
        out.push(IxEntry::mark(
            store::IX_CLOSED_TICKETS,
            Key::new()
                .s(&t.workspace)
                .s(closed_at)
                .s(&t.repo)
                .u(t.number)
                .bytes(),
        ));
    }
    out
}

pub fn pull_index(p: &Pull) -> Vec<IxEntry> {
    let mut out = Vec::new();
    if p.state == "open" {
        out.push(IxEntry {
            table: store::IX_OPEN_PULLS,
            key: Pull::open_key(&p.repo, &p.head, &p.base),
            value: p.number.to_string().into_bytes(),
        });
        for m in p.involved() {
            out.push(IxEntry::mark(
                store::IX_INVOLVED,
                Key::new().s(&m).s(&p.repo).u(p.number).bytes(),
            ));
        }
    }
    for link in &p.tickets {
        if let Some((repo, number)) = &link.local {
            out.push(IxEntry::mark(
                store::IX_TICKET_PULLS,
                Key::new().s(repo).u(*number).s(&p.repo).u(p.number).bytes(),
            ));
        }
    }
    if let (Some(closed_at), "closed") = (&p.closed_at, p.state.as_str()) {
        out.push(IxEntry::mark(
            store::IX_CLOSED_PULLS,
            Key::new().s(&p.repo).s(closed_at).u(p.number).bytes(),
        ));
    }
    out
}

pub fn entry_index(id: &str, kind: &str, owner: &str, table_key: &[u8]) -> Vec<IxEntry> {
    let entry = EntryRef {
        kind: kind.to_string(),
        owner: owner.to_string(),
        key: crate::util::b64(table_key),
    };
    vec![IxEntry {
        table: store::IX_ENTRIES,
        key: Key::new().s(id).bytes(),
        value: serde_json::to_vec(&entry).unwrap_or_default(),
    }]
}

pub fn idempotency_index(member: &str, key: &str, r: &IdempotencyRecord) -> Vec<IxEntry> {
    vec![IxEntry::mark(
        store::IX_IDEMPOTENCY_TIME,
        Key::new().s(&r.created_at).s(member).s(key).bytes(),
    )]
}

/// Replaces the index entries of a record: removes those of `old` that `new`
/// does not have and inserts the rest of `new`.
pub fn reindex(w: &Wtx, old: Vec<IxEntry>, new: Vec<IxEntry>) -> AppResult<()> {
    for o in &old {
        if !new.iter().any(|n| n.same(o)) {
            w.del(o.table, &o.key)?;
        }
    }
    for n in &new {
        if !old.iter().any(|o| o.same(n)) {
            w.put_raw(n.table, &n.key, &n.value)?;
        }
    }
    Ok(())
}

/// Writes a record and keeps its derived indexes in the same transaction.
pub fn put_indexed<T: Serialize>(
    w: &Wtx,
    table: Tbl,
    key: &[u8],
    old: Option<Vec<IxEntry>>,
    value: &T,
    new: Vec<IxEntry>,
) -> AppResult<()> {
    w.put(table, key, value)?;
    reindex(w, old.unwrap_or_default(), new)
}

/// Every index entry the primary tables imply.
pub fn expected_index(r: &impl Rd) -> AppResult<Vec<IxEntry>> {
    let mut out = Vec::new();
    for t in r.prefix_all::<Token>(store::TOKENS, &[])? {
        out.extend(token_index(&t));
    }
    for x in r.prefix_all::<Repo>(store::REPOS, &[])? {
        out.extend(repo_index(&x));
    }
    for x in r.prefix_all::<Workspace>(store::WORKSPACES, &[])? {
        out.extend(workspace_index(&x));
    }
    for x in r.prefix_all::<Ticket>(store::TICKETS, &[])? {
        out.extend(ticket_index(&x));
    }
    for x in r.prefix_all::<Pull>(store::PULLS, &[])? {
        out.extend(pull_index(&x));
    }
    for (key, value) in r.prefix_raw(store::COMMENTS, &[], None, usize::MAX)? {
        let c: Comment = store::decode(&value)?;
        let owner = comment_owner(&key).unwrap_or_default();
        out.extend(entry_index(&c.id, "comment", &owner, &key));
    }
    for (key, value) in r.prefix_raw(store::REVIEWS, &[], None, usize::MAX)? {
        let x: Review = store::decode(&value)?;
        let owner = pull_owner(&key, "suu").unwrap_or_default();
        out.extend(entry_index(&x.id, "review", &owner, &key));
    }
    for (key, value) in r.prefix_raw(store::INLINE, &[], None, usize::MAX)? {
        let x: InlineComment = store::decode(&value)?;
        let owner = pull_owner(&key, "suuu").unwrap_or_default();
        out.extend(entry_index(&x.id, "inline", &owner, &key));
    }
    for (key, value) in r.prefix_raw(store::IDEMPOTENCY, &[], None, usize::MAX)? {
        let x: IdempotencyRecord = store::decode(&value)?;
        if let Some(parts) = store::decode_key(&key, "ss") {
            out.extend(idempotency_index(parts[0].s(), parts[1].s(), &x));
        }
    }
    out.extend(crate::ci::queue::expected_index(r)?);
    Ok(out)
}

fn comment_owner(key: &[u8]) -> Option<String> {
    let parts = store::decode_key(key, "ssuu")?;
    Some(format!(
        "{}:{}:{}",
        parts[0].s(),
        parts[1].s(),
        parts[2].u()
    ))
}

fn pull_owner(key: &[u8], kinds: &str) -> Option<String> {
    let parts = store::decode_key(key, kinds)?;
    Some(format!("pull:{}:{}", parts[0].s(), parts[1].u()))
}

/// Differences between the stored indexes and the expected ones.
pub fn index_problems(r: &impl Rd) -> AppResult<Vec<String>> {
    let expected = expected_index(r)?;
    let mut problems = Vec::new();
    for table in store::INDEX_TABLES {
        let actual = r.prefix_raw(*table, &[], None, usize::MAX)?;
        let wanted: Vec<&IxEntry> = expected
            .iter()
            .filter(|e| e.table.name() == table.name())
            .collect();
        for (k, v) in &actual {
            if !wanted.iter().any(|e| &e.key == k && &e.value == v) {
                problems.push(format!(
                    "{}: stale entry {}",
                    table.name(),
                    crate::util::hex(k)
                ));
            }
        }
        for e in &wanted {
            if !actual.iter().any(|(k, v)| k == &e.key && v == &e.value) {
                problems.push(format!(
                    "{}: missing entry {}",
                    table.name(),
                    crate::util::hex(&e.key)
                ));
            }
        }
    }
    // Two open pull requests of one head and base break the uniqueness the
    // open-pull index enforces.
    let mut seen = std::collections::HashSet::new();
    for e in expected
        .iter()
        .filter(|e| e.table.name() == store::IX_OPEN_PULLS.name())
    {
        if !seen.insert(e.key.clone()) {
            problems.push("ix_open_pulls: two open pull requests share a head and base".into());
        }
    }
    Ok(problems)
}

/// Drops and rebuilds every derived index.
pub fn rebuild_indexes(w: &Wtx) -> AppResult<usize> {
    for table in store::INDEX_TABLES {
        w.clear(*table)?;
    }
    let expected = expected_index(w)?;
    for e in &expected {
        w.put_raw(e.table, &e.key, &e.value)?;
    }
    Ok(expected.len())
}
