# Editor restoration admission

Date: 2026-10-29

_Anchors:_ `crates/editor/src/items.rs::AUTO_RESTORE_FILE_SIZE_LIMIT`, `crates/editor/src/items.rs::notify_restore_size_limit`, `crates/project/src/project.rs::Project::open_buffer_with_file_size_limit`, `crates/worktree/src/worktree.rs::check_file_size`, `crates/workspace/src/persistence/model.rs::SerializedPane::deserialize_to`

- **Restoration admission**: a caller-scoped byte budget for automatically restored local files without serialized recovery text. The editor owns the policy; the worktree enforces it before reading and before full decoding. Explicit opens keep their existing policy.
- **Recovery text**: persisted unsaved contents. Its presence exempts that editor from this new refusal, including when recovery display is disabled. This avoids deleting recovery records through failed-item cleanup.
- **Admission identity**: the path and byte budget of a pending local load. Different policies must not share a refusal. Successful concurrent loads still publish one shared buffer per path.
- **Surviving tab state**: active, preview, and pinned state belongs to successfully restored items, not their old indices before a refusal.

The user approved this first safety slice in [Vikunja #1590](https://vikunja.wint3rmute.com/tasks/1590). It is not a global memory ceiling or a virtualized large-file editor. Recovery loads, explicit opens, and concurrent smaller files can still use substantial memory.

The bounded regression tests live in `crates/editor/src/items.rs`. Fake metadata and read counters live in `crates/fs/src/fs.rs::FakeFs`. No test needs the real SQL dump or the live workspace database.

Separate investigations: [ACP copies and retention #1591](https://vikunja.wint3rmute.com/tasks/1591), [helper accounting #1592](https://vikunja.wint3rmute.com/tasks/1592), and [the later memory event #1593](https://vikunja.wint3rmute.com/tasks/1593).
