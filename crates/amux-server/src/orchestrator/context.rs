//! Context assembly pipeline + snapshot recording (RR-0070, Invariant 27).
//!
//! `assemble_context` builds the layered context a worker receives on
//! assignment, as `ContextFragment`s whose `priority` IS the pipeline
//! position (lower = earlier in the assembled context):
//!
//! | priority | source          | layer                                       |
//! |----------|-----------------|---------------------------------------------|
//! | 0        | memory:org      | org-scope memories                          |
//! | 10       | memory:global   | global instructions (Global-scope memories) |
//! | 20       | memory:group    | the worker's group memories                 |
//! | 30       | memory:worker   | the worker's private memories               |
//! | 40       | task            | task context: title / desc / deps           |
//! | 50       | turns           | prior-turn results (placeholder until the   |
//! |          |                 | turn ledger, RR-0013 wiring, lands)         |
//!
//! Ordering note: this is the plan's ASSEMBLY order (standing instructions,
//! general -> specific, then the task at hand, then recent results). RR-0070
//! also states a priority list "task > deps > memory > turns > history" —
//! that is IMPORTANCE under a token budget (what survives trimming), not
//! position in the prompt; budget-driven trimming lands with Invariant 16's
//! token budgets and does not exist yet, so nothing here trims.
//!
//! Which memories appear is decided by core's ONE visibility predicate
//! (`amux_core::memory::visible` via `db::memories::list_visible`,
//! Invariant 2): a worker-scoped memory reaches only that worker's context.
//!
//! `record_snapshot` persists the snapshot per assignment, INSERT OR IGNORE
//! on the planner's idempotency key — recording is idempotent (Invariant 27:
//! the snapshot is immutable; a re-planned assignment re-records nothing).

use crate::db::memories;
use amux_core::board::Task;
use amux_core::ids::{GroupId, TaskId, WorkerId};
use amux_core::scope::{ResolutionTarget, ScopeLevel};
use amux_core::turn::{ContextFragment, ContextSnapshot};
use rusqlite::{params, Connection};

/// Pipeline positions (see module table). Public so a future budget-trimming
/// pass and the dashboard speak the same numbers instead of re-inventing
/// them.
pub const PRIO_MEMORY_ORG: u32 = 0;
pub const PRIO_MEMORY_GLOBAL: u32 = 10;
pub const PRIO_MEMORY_GROUP: u32 = 20;
pub const PRIO_MEMORY_WORKER: u32 = 30;
pub const PRIO_TASK: u32 = 40;
pub const PRIO_TURNS: u32 = 50;

fn memory_layer(level: ScopeLevel) -> (u32, &'static str) {
    match level {
        ScopeLevel::Org => (PRIO_MEMORY_ORG, "memory:org"),
        ScopeLevel::Global => (PRIO_MEMORY_GLOBAL, "memory:global"),
        ScopeLevel::Group => (PRIO_MEMORY_GROUP, "memory:group"),
        ScopeLevel::Worker => (PRIO_MEMORY_WORKER, "memory:worker"),
    }
}

/// Assemble the context snapshot for assigning `task` to `worker`.
///
/// Deterministic by construction: every input is read from the DB in this
/// one call, fragment content is a pure function of those rows, and
/// `ContextSnapshot::build` canonicalizes order before hashing — same DB
/// state, same (worker, task), same hash, every time (Invariant 27's cache
/// rule depends on exactly this).
///
/// Returns `rusqlite::Result` rather than the plan sketch's bare snapshot:
/// a read that fails must surface, not vanish into an empty context that
/// hashes as if the worker legitimately received nothing (ethos rule 7 — a
/// wrong answer that looks plausible is worse than an error).
pub fn assemble_context(
    conn: &Connection,
    worker: &WorkerId,
    task: &Task,
) -> rusqlite::Result<ContextSnapshot> {
    // Resolution target: the worker plus its group (looked up from the
    // worker row; a worker with no row still resolves as itself + globals).
    let group: Option<GroupId> = crate::db::queries::get_worker(conn, worker.as_str())?
        .and_then(|row| row.group_id)
        .and_then(|g| GroupId::parse(&g).ok());
    let target = ResolutionTarget {
        worker: Some(worker.clone()),
        group,
    };

    let mut fragments: Vec<ContextFragment> = Vec::new();

    // Memory layers: org -> global -> group -> worker (general to specific,
    // so the more specific layer lands closer to the task and can override
    // in the model's reading — the same precedence direction as Invariant 2).
    for e in memories::list_visible(conn, &target)? {
        let (priority, source) = memory_layer(e.scope.level());
        fragments.push(ContextFragment {
            priority,
            source: source.into(),
            content: format!("{}: {}", e.name, e.content),
        });
    }

    // Task context: title / desc / deps in ONE fragment — the unit the
    // worker is being handed.
    let mut task_content = format!("Task {}: {}", task.id, task.title);
    if !task.desc.trim().is_empty() {
        task_content.push('\n');
        task_content.push_str(&task.desc);
    }
    if !task.depends_on.is_empty() {
        let deps: Vec<&str> = task.depends_on.iter().map(|d| d.as_str()).collect();
        task_content.push_str("\nDepends on: ");
        task_content.push_str(&deps.join(", "));
    }
    fragments.push(ContextFragment {
        priority: PRIO_TASK,
        source: "task".into(),
        content: task_content,
    });

    // Prior-turn results: a fixed placeholder until the turn ledger lands
    // (RR-0013 wiring). Fixed TEXT, not an omitted fragment, so the layer's
    // absence-of-data is explicit in the snapshot a human reads AND the
    // hash moves when real turn results replace it (an omission must
    // announce itself, Invariant 40).
    fragments.push(ContextFragment {
        priority: PRIO_TURNS,
        source: "turns".into(),
        content: "(prior-turn results: none recorded — turn ledger lands with RR-0013 wiring)"
            .into(),
    });

    Ok(ContextSnapshot::build(fragments))
}

/// Record a snapshot for one assignment (Invariant 27). INSERT OR IGNORE on
/// the UNIQUE `assignment_key` — the planner's idempotency key — so
/// re-recording the same assignment is a no-op. Returns whether a row was
/// NEWLY recorded (false = this assignment already has its immutable
/// snapshot; the caller must not bump revisions or emit events for it).
pub fn record_snapshot(
    conn: &Connection,
    assignment_key: &str,
    task_id: &TaskId,
    worker_id: &WorkerId,
    snap: &ContextSnapshot,
) -> rusqlite::Result<bool> {
    let fragments = serde_json::to_string(&snap.fragments).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(e))
    })?;
    let n = conn.execute(
        "INSERT OR IGNORE INTO _amux_context_snapshots
         (assignment_key, task_id, worker_id, content_hash, fragments, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            assignment_key,
            task_id.as_str(),
            worker_id.as_str(),
            snap.content_hash,
            fragments,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(n > 0)
}

/// Find the board task behind a planner TaskId. Board tasks carry synthetic
/// ids minted by `board_store::internal_id` (a one-way hash of the semantic
/// id), so the lookup scans and re-mints — the table is board-sized and the
/// scan is exact, where a reverse map would be a second spelling of the id
/// scheme that could drift from the first.
pub fn task_by_internal_id(conn: &Connection, id: &TaskId) -> rusqlite::Result<Option<Task>> {
    Ok(issue_by_internal_id(conn, id)?.and_then(|row| row.to_task()))
}

/// The RAW issue row behind a planner TaskId — for callers that must write
/// board columns (log, rev) rather than reason over the core Task. Same
/// scan-and-re-mint as [`task_by_internal_id`], one implementation.
pub fn issue_by_internal_id(
    conn: &Connection,
    id: &TaskId,
) -> rusqlite::Result<Option<crate::db::board_store::IssueRow>> {
    let rows = crate::db::board_store::list_issues(
        conn,
        &[],
        &[],
        crate::db::board_store::ArchivedFilter::All,
    )?;
    Ok(rows
        .into_iter()
        .find(|row| crate::db::board_store::internal_id(&row.id) == *id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use amux_core::events::Actor;
    use amux_core::ids::MemoryId;
    use amux_core::memory::{MemoryEntry, MemoryProvenance, MemoryType};
    use amux_core::scope::Scope;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        crate::db::migrate::apply_all(&mut c).unwrap();
        c
    }

    fn wid(n: u64) -> WorkerId {
        WorkerId::from_ulid(ulid::Ulid::from_parts(1_700_000_000_000, n as u128))
    }

    fn gid(n: u64) -> GroupId {
        GroupId::from_ulid(ulid::Ulid::from_parts(1_700_000_000_000, n as u128))
    }

    fn tid(n: u64) -> TaskId {
        TaskId::from_ulid(ulid::Ulid::from_parts(1_700_000_000_000, n as u128))
    }

    fn seed_memory(c: &Connection, n: u64, scope: Scope, name: &str, content: &str) {
        let e = MemoryEntry::new(
            MemoryId::from_ulid(ulid::Ulid::from_parts(1_700_000_000_000, n as u128)),
            scope,
            name,
            content,
            MemoryType::Project,
            MemoryProvenance::HumanWritten,
            "2026-08-01T00:00:00Z".parse().unwrap(),
        );
        crate::db::memories::insert(c, &e).unwrap();
    }

    /// A worker row with a group, so assemble_context resolves group scope.
    fn seed_worker_in_group(c: &Connection, w: &WorkerId, g: &GroupId) {
        c.execute(
            "INSERT INTO _amux_workers (id, display_name, group_id, created_at, updated_at)
             VALUES (?1, 'w', ?2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            params![w.as_str(), g.as_str()],
        )
        .unwrap();
    }

    fn task(n: u64) -> Task {
        let mut t = Task::create(
            tid(n),
            "Fix the login redirect loop",
            amux_core::board::ItemType::Code,
            Actor::Human { name: "ethan".into() },
            "2026-08-01T00:00:00Z".parse().unwrap(),
        );
        t.desc = "Users bounce between /login and /".into();
        t.depends_on = vec![tid(900)];
        t
    }

    #[test]
    fn layers_ordered_global_before_group_before_worker_then_task() {
        let c = conn();
        let (w, g) = (wid(1), gid(2));
        seed_worker_in_group(&c, &w, &g);
        seed_memory(&c, 10, Scope::Global, "house-rules", "no em-dashes");
        seed_memory(&c, 11, Scope::Group { id: g.clone() }, "team-notes", "deploy fridays");
        seed_memory(&c, 12, Scope::Worker { id: w.clone() }, "my-notes", "auth in src/auth.rs");
        // Another worker's memory must NOT appear in this context.
        seed_memory(&c, 13, Scope::Worker { id: wid(99) }, "not-mine", "secret");

        let snap = assemble_context(&c, &w, &task(1)).unwrap();
        let sources: Vec<&str> = snap.fragments.iter().map(|f| f.source.as_str()).collect();
        assert_eq!(
            sources,
            vec!["memory:global", "memory:group", "memory:worker", "task", "turns"],
            "assembly order: global -> group -> worker -> task -> turns"
        );
        // Priorities are strictly the documented pipeline positions.
        let prios: Vec<u32> = snap.fragments.iter().map(|f| f.priority).collect();
        assert_eq!(
            prios,
            vec![PRIO_MEMORY_GLOBAL, PRIO_MEMORY_GROUP, PRIO_MEMORY_WORKER, PRIO_TASK, PRIO_TURNS]
        );
        // Isolation: the other worker's memory is absent.
        assert!(!snap.fragments.iter().any(|f| f.content.contains("secret")));
        // Task fragment carries title, desc and deps.
        let task_frag = &snap.fragments[3];
        assert!(task_frag.content.contains("Fix the login redirect loop"));
        assert!(task_frag.content.contains("Users bounce"));
        assert!(task_frag.content.contains(tid(900).as_str()));
    }

    #[test]
    fn same_inputs_same_hash_different_inputs_different_hash() {
        let c = conn();
        let w = wid(1);
        seed_memory(&c, 10, Scope::Global, "house-rules", "no em-dashes");

        let a = assemble_context(&c, &w, &task(1)).unwrap();
        let b = assemble_context(&c, &w, &task(1)).unwrap();
        assert_eq!(a.content_hash, b.content_hash, "deterministic");
        assert_eq!(a, b);

        // A new visible memory moves the hash.
        seed_memory(&c, 11, Scope::Worker { id: w.clone() }, "note", "x");
        let d = assemble_context(&c, &w, &task(1)).unwrap();
        assert_ne!(d.content_hash, a.content_hash);
    }

    #[test]
    fn record_snapshot_is_idempotent_per_assignment_key() {
        let c = conn();
        let w = wid(1);
        let t = task(1);
        let snap = assemble_context(&c, &w, &t).unwrap();
        let key = format!("{}:{}:1", t.id, w);

        assert!(record_snapshot(&c, &key, &t.id, &w, &snap).unwrap(), "first: recorded");
        assert!(
            !record_snapshot(&c, &key, &t.id, &w, &snap).unwrap(),
            "second: same key, NOT re-recorded"
        );
        let (n, hash): (i64, String) = c
            .query_row(
                "SELECT COUNT(*), MAX(content_hash) FROM _amux_context_snapshots
                 WHERE assignment_key = ?1",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(hash, snap.content_hash);

        // A NEW attempt is a new assignment key -> its own row.
        let key2 = format!("{}:{}:2", t.id, w);
        assert!(record_snapshot(&c, &key2, &t.id, &w, &snap).unwrap());
    }
}
