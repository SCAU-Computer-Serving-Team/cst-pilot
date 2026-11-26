use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ntfs_ng::{
    FileReference, records,
    scan::{Chunk, Entry as ScanEntry, Projection, Scan},
    source::ReadAt,
    types,
    value::{FileName, StandardInformation},
};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use crate::diagnostics::mft::mft_enabled;
use crate::diagnostics::volume_index::{MemoryReservation, ProcessMemoryLedger, VolumeIdentity};
use crate::diagnostics::walk::is_cloud_reparse_tag;

use super::super::path_guard::{PathGuardSession, SearchGuardError, VerifiedSearchEntry, path_key};
use super::walk_engine::WalkQuerySpec;
use super::{
    FffindKind, FindCandidate, cmp_find_candidate, ext_matches, format_local_rfc3339,
    normalize_path, path_within_modified_window, size_within_bounds,
};

pub(crate) const MAX_MFT_SEARCH_RECORDS: u64 = 4_000_000;
pub(crate) const MAX_VOLUME_INDEX_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const IDLE_TTL: Duration = Duration::from_secs(180);
const MAX_CONCURRENT_MFT_BUILDS: usize = 2;
const MAX_TOMBSTONE_NUM: usize = 1;
const MAX_TOMBSTONE_DEN: usize = 5; // 20% tombstone ratio triggers clean rebuild.

const FLAG_IS_DIR: u16 = 1 << 0;
const FLAG_IS_REPARSE: u16 = 1 << 1;
const FLAG_IS_CLOUD_HYDRATED: u16 = 1 << 2;
const FLAG_META_STALE: u16 = 1 << 3;
const FLAG_TOMBSTONE: u16 = 1 << 4;

const EDGE_FLAG_TOMBSTONE: u8 = 1 << 0;
const EDGE_FLAG_LOSSY_NAME: u8 = 1 << 1;

pub(crate) const USN_REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub(crate) const USN_REASON_DATA_EXTEND: u32 = 0x0000_0002;
pub(crate) const USN_REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
pub(crate) const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub(crate) const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub(crate) const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub(crate) const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub(crate) const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub(crate) const USN_REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;
pub(crate) const USN_REASON_REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
pub(crate) const USN_REASON_CLOSE: u32 = 0x8000_0000;

/// Compact 32-byte per-object node keyed by full 64-bit NTFS `FileReference`
/// (48-bit record number + 16-bit sequence number).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ObjectNode {
    pub file_ref: u64,
    pub size_bytes: u64,
    pub mtime_secs: i64,
    pub flags: u16,
    pub edge_count: u16,
    pub first_edge: u32,
}

/// Separate directory name edge preserving all real hard links while excluding DOS 8.3 aliases.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NameEdge {
    pub parent_ref: u64,
    pub obj_idx: u32,
    pub name_offset: u32,
    pub name_len: u16,
    #[allow(dead_code)]
    pub namespace: u8,
    pub flags: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsnEventV2 {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub usn: i64,
    pub reason: u32,
    pub file_attributes: u32,
    pub name_utf8: String,
    pub name_lossless: bool,
}

#[derive(Clone, Debug)]
struct ReachabilityState {
    tree_gen: u64,
    /// Per-object reachability: `Some(depth)` if reachable and unpruned from root,
    /// `None` if orphan, cycle, parent-sequence mismatch, reparse dir, or `.git`.
    dir_depth: Vec<Option<u16>>,
    /// Per-object canonical parent edge index chosen for fast path reconstruction.
    dir_parent_edge: Vec<Option<u32>>,
}

#[derive(Debug)]
pub(crate) struct VolumeFileIndex {
    pub drive_upper: String,
    pub identity: VolumeIdentity,
    pub journal_id: u64,
    pub applied_cursor: i64,
    pub root_ref: u64,
    pub objects: Vec<ObjectNode>,
    pub edges: Vec<NameEdge>,
    pub name_arena: Vec<u8>,
    pub ref_to_obj: HashMap<u64, u32>,
    pub live_objects: usize,
    pub tombstones: usize,
    reachability: ReachabilityState,
    pub estimated_bytes: usize,
    /// RAII reservation in the unified 384 MiB process memory ledger.
    /// Lives as long as this `Arc<VolumeFileIndex>` generation is held by cache or in-flight readers.
    #[allow(dead_code)]
    mem_guard: Option<MemoryReservation>,
}

impl VolumeFileIndex {
    pub(crate) fn compute_estimated_bytes(
        objects: &Vec<ObjectNode>,
        edges: &Vec<NameEdge>,
        name_arena: &Vec<u8>,
        ref_to_obj: &HashMap<u64, u32>,
    ) -> usize {
        objects
            .capacity()
            .saturating_mul(std::mem::size_of::<ObjectNode>())
            .saturating_add(
                edges
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NameEdge>()),
            )
            .saturating_add(name_arena.capacity())
            .saturating_add(ref_to_obj.capacity().saturating_mul(24))
            .saturating_add(objects.capacity().saturating_mul(8))
    }

    pub(crate) fn clone_with_reservation(&self, ledger: &Arc<ProcessMemoryLedger>) -> Option<Self> {
        let need = Self::compute_estimated_bytes(
            &self.objects,
            &self.edges,
            &self.name_arena,
            &self.ref_to_obj,
        );
        let guard = ledger.try_reserve(need)?;
        Some(Self {
            drive_upper: self.drive_upper.clone(),
            identity: self.identity.clone(),
            journal_id: self.journal_id,
            applied_cursor: self.applied_cursor,
            root_ref: self.root_ref,
            objects: self.objects.clone(),
            edges: self.edges.clone(),
            name_arena: self.name_arena.clone(),
            ref_to_obj: self.ref_to_obj.clone(),
            live_objects: self.live_objects,
            tombstones: self.tombstones,
            reachability: self.reachability.clone(),
            estimated_bytes: need,
            mem_guard: Some(guard),
        })
    }

    pub(crate) fn edge_name(&self, edge: &NameEdge) -> &str {
        let start = edge.name_offset as usize;
        let end = start.saturating_add(edge.name_len as usize);
        if end <= self.name_arena.len() {
            std::str::from_utf8(&self.name_arena[start..end]).unwrap_or("")
        } else {
            ""
        }
    }

    pub(crate) fn tombstone_ratio_exceeded(&self) -> bool {
        let total = self.live_objects.saturating_add(self.tombstones);
        total > 1024 && self.tombstones * MAX_TOMBSTONE_DEN > total * MAX_TOMBSTONE_NUM
    }

    /// Recomputes reachability, depth, and `.git`/reparse pruning for all directory objects
    /// bound to the incremented `tree_gen`. Rejects orphans, cycles, and sequence mismatches.
    pub(crate) fn recompute_reachability(&mut self) {
        let next_gen = self.reachability.tree_gen.wrapping_add(1).max(1);
        let n = self.objects.len();
        let mut dir_depth: Vec<Option<u16>> = vec![None; n];
        let mut dir_parent_edge: Vec<Option<u32>> = vec![None; n];

        // Pick one primary live edge per directory object (directories have a single real parent).
        for (edge_idx, edge) in self.edges.iter().enumerate() {
            if (edge.flags & EDGE_FLAG_TOMBSTONE) != 0 {
                continue;
            }
            let obj_idx = edge.obj_idx as usize;
            if obj_idx >= n {
                continue;
            }
            let obj = &self.objects[obj_idx];
            if (obj.flags & FLAG_TOMBSTONE) != 0 || (obj.flags & FLAG_IS_DIR) == 0 {
                continue;
            }
            if dir_parent_edge[obj_idx].is_none() {
                dir_parent_edge[obj_idx] = Some(edge_idx as u32);
            }
        }

        // Root directory is depth 0.
        if let Some(&root_idx) = self.ref_to_obj.get(&self.root_ref) {
            let ri = root_idx as usize;
            if ri < n {
                dir_depth[ri] = Some(0);
            }
        }

        // Resolve depth & policy for each directory with cycle detection (`0 = unvisited`, `1 = visiting`, `2 = done`).
        let mut state = vec![0u8; n];
        if let Some(&root_idx) = self.ref_to_obj.get(&self.root_ref)
            && (root_idx as usize) < n
        {
            state[root_idx as usize] = 2;
        }
        let mut chain: Vec<usize> = Vec::with_capacity(64);
        for start in 0..n {
            if state[start] == 2 {
                continue;
            }
            let obj = &self.objects[start];
            if (obj.flags & FLAG_TOMBSTONE) != 0 || (obj.flags & FLAG_IS_DIR) == 0 {
                state[start] = 2;
                continue;
            }
            chain.clear();
            let mut cur = start;
            let resolved_base: Option<u16> = loop {
                if state[cur] == 2 {
                    break dir_depth[cur];
                }
                if state[cur] == 1 {
                    // Cycle detected -> prune entire chain.
                    break None;
                }
                state[cur] = 1;
                chain.push(cur);

                let cur_obj = &self.objects[cur];
                if (cur_obj.flags & (FLAG_TOMBSTONE | FLAG_IS_REPARSE)) != 0 {
                    break None;
                }
                let Some(edge_idx) = dir_parent_edge[cur] else {
                    break None;
                };
                let edge = &self.edges[edge_idx as usize];
                let name = self.edge_name(edge);
                if name.is_empty() || name.eq_ignore_ascii_case(".git") {
                    break None;
                }
                if edge.parent_ref == self.root_ref {
                    break Some(0);
                }
                // Full 64-bit FileReference lookup enforces exact parent sequence match!
                let Some(&parent_idx) = self.ref_to_obj.get(&edge.parent_ref) else {
                    break None;
                };
                let pi = parent_idx as usize;
                if pi >= n || (self.objects[pi].flags & FLAG_IS_DIR) == 0 {
                    break None;
                }
                cur = pi;
            };

            let mut depth_opt = resolved_base;
            for &node_idx in chain.iter().rev() {
                state[node_idx] = 2;
                if let Some(d) = depth_opt {
                    let next_d = d.saturating_add(1);
                    if next_d <= 1024 {
                        dir_depth[node_idx] = Some(next_d);
                        depth_opt = Some(next_d);
                    } else {
                        dir_depth[node_idx] = None;
                        depth_opt = None;
                    }
                } else {
                    dir_depth[node_idx] = None;
                }
            }
        }

        self.reachability = ReachabilityState {
            tree_gen: next_gen,
            dir_depth,
            dir_parent_edge,
        };
    }

    /// Reconstruct relative path from volume root (with `/` separators) for a directory object.
    fn dir_rel_path(&self, dir_idx: usize) -> Option<String> {
        let depth = self
            .reachability
            .dir_depth
            .get(dir_idx)
            .copied()
            .flatten()?;
        if depth == 0 {
            return Some(String::new());
        }
        let mut parts: Vec<&str> = Vec::with_capacity(depth as usize);
        let mut cur = dir_idx;
        for _ in 0..=depth {
            let edge_idx = self
                .reachability
                .dir_parent_edge
                .get(cur)
                .copied()
                .flatten()?;
            let edge = &self.edges[edge_idx as usize];
            parts.push(self.edge_name(edge));
            if edge.parent_ref == self.root_ref {
                break;
            }
            cur = *self.ref_to_obj.get(&edge.parent_ref)? as usize;
        }
        parts.reverse();
        Some(parts.join("/"))
    }

    /// Apply a batch of USN V2 events onto a cloned mutable generation.
    /// Returns `Ok(())` or `Err(())` if index must be rebuilt.
    pub(crate) fn apply_usn_events(
        &mut self,
        events: &[UsnEventV2],
        new_cursor: i64,
    ) -> Result<(), ()> {
        let mut tree_dirty = false;
        let mut rename_old_seen: HashSet<u64> = HashSet::new();

        // Build per-object live edge index for O(1) edge lookup during batch replay.
        let mut obj_edges: HashMap<u32, Vec<usize>> = HashMap::new();
        if events.len() > 8 {
            for (idx, edge) in self.edges.iter().enumerate() {
                if (edge.flags & EDGE_FLAG_TOMBSTONE) == 0 {
                    obj_edges.entry(edge.obj_idx).or_default().push(idx);
                }
            }
        }
        let use_edge_map = events.len() > 8;

        for ev in events {
            if !ev.name_lossless
                && (ev.reason & (USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME)) != 0
            {
                continue;
            }
            let is_dir = (ev.file_attributes & 0x10) != 0;
            let is_reparse = (ev.file_attributes & 0x400) != 0;

            if (ev.reason & USN_REASON_FILE_DELETE) != 0 {
                if let Some(&obj_idx) = self.ref_to_obj.get(&ev.file_ref) {
                    let oi = obj_idx as usize;
                    if oi < self.objects.len() && (self.objects[oi].flags & FLAG_TOMBSTONE) == 0 {
                        let mut remaining_edges = 0usize;
                        if use_edge_map {
                            if let Some(list) = obj_edges.get_mut(&obj_idx) {
                                list.retain(|&ei| {
                                    let edge = &mut self.edges[ei];
                                    if (edge.flags & EDGE_FLAG_TOMBSTONE) != 0 {
                                        return false;
                                    }
                                    if edge.parent_ref == ev.parent_ref
                                        && Self::edge_name_eq_arena(
                                            &self.name_arena,
                                            edge,
                                            &ev.name_utf8,
                                        )
                                    {
                                        edge.flags |= EDGE_FLAG_TOMBSTONE;
                                        false
                                    } else {
                                        remaining_edges += 1;
                                        true
                                    }
                                });
                            }
                        } else {
                            for edge in self.edges.iter_mut() {
                                if edge.obj_idx == obj_idx
                                    && (edge.flags & EDGE_FLAG_TOMBSTONE) == 0
                                {
                                    if edge.parent_ref == ev.parent_ref
                                        && Self::edge_name_eq_arena(
                                            &self.name_arena,
                                            edge,
                                            &ev.name_utf8,
                                        )
                                    {
                                        edge.flags |= EDGE_FLAG_TOMBSTONE;
                                    } else {
                                        remaining_edges += 1;
                                    }
                                }
                            }
                        }
                        if remaining_edges == 0 {
                            self.objects[oi].flags |= FLAG_TOMBSTONE;
                            self.ref_to_obj.remove(&ev.file_ref);
                            self.live_objects = self.live_objects.saturating_sub(1);
                            self.tombstones += 1;
                            if is_dir || (self.objects[oi].flags & FLAG_IS_DIR) != 0 {
                                tree_dirty = true;
                            }
                        }
                    }
                }
                continue;
            }

            if (ev.reason & USN_REASON_RENAME_OLD_NAME) != 0 {
                rename_old_seen.insert(ev.file_ref);
                if let Some(&obj_idx) = self.ref_to_obj.get(&ev.file_ref) {
                    if use_edge_map {
                        if let Some(list) = obj_edges.get_mut(&obj_idx) {
                            list.retain(|&ei| {
                                let edge = &mut self.edges[ei];
                                if (edge.flags & EDGE_FLAG_TOMBSTONE) != 0 {
                                    return false;
                                }
                                if edge.parent_ref == ev.parent_ref
                                    && Self::edge_name_eq_arena(
                                        &self.name_arena,
                                        edge,
                                        &ev.name_utf8,
                                    )
                                {
                                    edge.flags |= EDGE_FLAG_TOMBSTONE;
                                    false
                                } else {
                                    true
                                }
                            });
                        }
                    } else {
                        for edge in self.edges.iter_mut() {
                            if edge.obj_idx == obj_idx
                                && (edge.flags & EDGE_FLAG_TOMBSTONE) == 0
                                && edge.parent_ref == ev.parent_ref
                                && Self::edge_name_eq_arena(&self.name_arena, edge, &ev.name_utf8)
                            {
                                edge.flags |= EDGE_FLAG_TOMBSTONE;
                            }
                        }
                    }
                    if is_dir {
                        tree_dirty = true;
                    }
                }
                continue;
            }

            // Hard link changes can remove or add links without naming the removed edge;
            // trigger a clean rebuild rather than risking ghost edges.
            if (ev.reason & USN_REASON_HARD_LINK_CHANGE) != 0 {
                return Err(());
            }

            if (ev.reason & (USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME)) != 0 {
                let existed_before = self.ref_to_obj.contains_key(&ev.file_ref);
                // Unpaired RENAME_NEW_NAME on an existing object without RENAME_OLD_NAME in batch
                // would leave a stale old edge behind -> trigger clean rebuild.
                if (ev.reason & USN_REASON_RENAME_NEW_NAME) != 0
                    && existed_before
                    && !rename_old_seen.contains(&ev.file_ref)
                {
                    return Err(());
                }
                let obj_idx = if let Some(&existing) = self.ref_to_obj.get(&ev.file_ref) {
                    existing
                } else {
                    if self.objects.len() as u64 >= MAX_MFT_SEARCH_RECORDS {
                        return Err(());
                    }
                    let idx = self.objects.len() as u32;
                    let mut flags = FLAG_META_STALE;
                    if is_dir {
                        flags |= FLAG_IS_DIR;
                    }
                    if is_reparse {
                        flags |= FLAG_IS_REPARSE;
                    }
                    self.objects.push(ObjectNode {
                        file_ref: ev.file_ref,
                        size_bytes: 0,
                        mtime_secs: 0,
                        flags,
                        edge_count: 0,
                        first_edge: u32::MAX,
                    });
                    self.ref_to_obj.insert(ev.file_ref, idx);
                    self.live_objects += 1;
                    idx
                };

                let oi = obj_idx as usize;
                self.objects[oi].flags |= FLAG_META_STALE;
                if is_dir {
                    self.objects[oi].flags |= FLAG_IS_DIR;
                    tree_dirty = true;
                }
                if is_reparse {
                    self.objects[oi].flags |= FLAG_IS_REPARSE;
                    if is_dir {
                        tree_dirty = true;
                    }
                } else if (ev.reason & USN_REASON_REPARSE_POINT_CHANGE) != 0 {
                    self.objects[oi].flags &= !FLAG_IS_REPARSE;
                    if is_dir {
                        tree_dirty = true;
                    }
                }

                // Append new NameEdge if not already present.
                if !ev.name_utf8.is_empty() {
                    let already = if use_edge_map {
                        obj_edges.get(&obj_idx).is_some_and(|list| {
                            list.iter().any(|&ei| {
                                let e = &self.edges[ei];
                                (e.flags & EDGE_FLAG_TOMBSTONE) == 0
                                    && e.parent_ref == ev.parent_ref
                                    && Self::edge_name_eq_arena(&self.name_arena, e, &ev.name_utf8)
                            })
                        })
                    } else {
                        self.edges.iter().any(|e| {
                            e.obj_idx == obj_idx
                                && (e.flags & EDGE_FLAG_TOMBSTONE) == 0
                                && e.parent_ref == ev.parent_ref
                                && Self::edge_name_eq_arena(&self.name_arena, e, &ev.name_utf8)
                        })
                    };
                    if !already {
                        let name_offset = u32::try_from(self.name_arena.len()).map_err(|_| ())?;
                        let name_len = u16::try_from(ev.name_utf8.len()).map_err(|_| ())?;
                        self.name_arena.extend_from_slice(ev.name_utf8.as_bytes());
                        let edge_idx = self.edges.len() as u32;
                        self.edges.push(NameEdge {
                            parent_ref: ev.parent_ref,
                            obj_idx,
                            name_offset,
                            name_len,
                            namespace: 1,
                            flags: 0,
                        });
                        if use_edge_map {
                            obj_edges
                                .entry(obj_idx)
                                .or_default()
                                .push(edge_idx as usize);
                        }
                        if self.objects[oi].first_edge == u32::MAX {
                            self.objects[oi].first_edge = edge_idx;
                        }
                        self.objects[oi].edge_count = self.objects[oi].edge_count.saturating_add(1);
                    }
                }
                rename_old_seen.remove(&ev.file_ref);
                continue;
            }

            // Data / basic info / reparse / close events invalidate cached metadata.
            if (ev.reason
                & (USN_REASON_DATA_OVERWRITE
                    | USN_REASON_DATA_EXTEND
                    | USN_REASON_DATA_TRUNCATION
                    | USN_REASON_BASIC_INFO_CHANGE
                    | USN_REASON_REPARSE_POINT_CHANGE
                    | USN_REASON_CLOSE))
                != 0
                && let Some(&obj_idx) = self.ref_to_obj.get(&ev.file_ref)
            {
                let oi = obj_idx as usize;
                self.objects[oi].flags |= FLAG_META_STALE;
                if (ev.reason & USN_REASON_REPARSE_POINT_CHANGE) != 0 {
                    if is_reparse {
                        self.objects[oi].flags |= FLAG_IS_REPARSE;
                    } else {
                        self.objects[oi].flags &= !FLAG_IS_REPARSE;
                    }
                    if is_dir {
                        tree_dirty = true;
                    }
                }
            }
        }

        // Half-rename without matching RENAME_NEW_NAME in batch -> trigger rebuild for safety.
        if !rename_old_seen.is_empty() {
            return Err(());
        }

        if tree_dirty || self.reachability.dir_depth.len() != self.objects.len() {
            self.recompute_reachability();
        }
        self.applied_cursor = new_cursor;
        self.estimated_bytes = Self::compute_estimated_bytes(
            &self.objects,
            &self.edges,
            &self.name_arena,
            &self.ref_to_obj,
        );
        if self.estimated_bytes > MAX_VOLUME_INDEX_BYTES || self.tombstone_ratio_exceeded() {
            return Err(());
        }
        Ok(())
    }

    fn edge_name_eq_arena(name_arena: &[u8], edge: &NameEdge, name: &str) -> bool {
        let start = edge.name_offset as usize;
        let end = start.saturating_add(edge.name_len as usize);
        end <= name_arena.len()
            && std::str::from_utf8(&name_arena[start..end])
                .is_ok_and(|s| s.eq_ignore_ascii_case(name))
    }

    /// Query this immutable `VolumeFileIndex` generation for a volume root or subtree.
    /// Every candidate eligible for the Top-K heap is verified on a single live handle
    /// via `PathGuardSession::verify_search_entry`, which also refreshes stale USN metadata!
    pub(crate) fn query(
        &self,
        session: &PathGuardSession,
        verified_root: &VerifiedSearchEntry,
        subtree_rel: Option<&str>,
        spec: &WalkQuerySpec,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<(Vec<FindCandidate>, bool, usize, bool), String> {
        let mut local: Vec<FindCandidate> =
            Vec::with_capacity(spec.limit.saturating_add(2).min(256));
        let mut truncated_by_limit = false;
        let mut skipped = 0usize;
        let mut scratch = String::with_capacity(256);

        let subtree_prefix_lower = subtree_rel
            .filter(|s| !s.is_empty())
            .map(|s| format!("{}/", s.trim_matches('/').to_ascii_lowercase()));
        let subtree_depth_base: u16 = subtree_rel
            .filter(|s| !s.is_empty())
            .map(|s| s.split('/').count() as u16)
            .unwrap_or(0);

        let mut dir_list_cache: HashMap<u32, bool> = HashMap::new();
        let mut timed_out = false;

        let cap = spec.limit.saturating_add(1);
        let sort_by = spec.sort_by;

        for (edge_idx, edge) in self.edges.iter().enumerate() {
            if edge_idx % 8192 == 0 {
                if cancel.is_cancelled() {
                    return Err("文件检索已取消。".into());
                }
                if Instant::now() >= deadline {
                    timed_out = true;
                    break;
                }
            }
            if (edge.flags & EDGE_FLAG_TOMBSTONE) != 0 {
                continue;
            }
            if (edge.flags & EDGE_FLAG_LOSSY_NAME) != 0 {
                skipped += 1;
                continue;
            }
            let obj_idx = edge.obj_idx as usize;
            let Some(obj) = self.objects.get(obj_idx) else {
                continue;
            };
            if (obj.flags & FLAG_TOMBSTONE) != 0 {
                continue;
            }
            let is_dir = (obj.flags & FLAG_IS_DIR) != 0;
            if is_dir && matches!(spec.kind, FffindKind::File) {
                continue;
            }
            if !is_dir && matches!(spec.kind, FffindKind::Dir) {
                continue;
            }
            if is_dir && (!spec.exts.is_empty() || spec.size_bounds.is_some()) {
                continue;
            }
            if is_dir && (obj.flags & FLAG_IS_REPARSE) != 0 {
                continue;
            }
            if !is_dir
                && (obj.flags & FLAG_IS_REPARSE) != 0
                && (obj.flags & FLAG_IS_CLOUD_HYDRATED) == 0
            {
                continue;
            }

            let name = self.edge_name(edge);
            if name.is_empty() || (is_dir && name.eq_ignore_ascii_case(".git")) {
                continue;
            }
            if !is_dir && !spec.exts.is_empty() && !ext_matches(name, &spec.exts) {
                continue;
            }
            // Fast pre-filter on non-stale metadata; stale entries pass through to live handle verification.
            let meta_stale = (obj.flags & FLAG_META_STALE) != 0;
            if !meta_stale {
                let mtime_opt = (obj.mtime_secs != 0).then_some(obj.mtime_secs);
                if !path_within_modified_window(mtime_opt, spec.modified_window) {
                    continue;
                }
                if !is_dir && !size_within_bounds(Some(obj.size_bytes), spec.size_bounds) {
                    continue;
                }
            }
            if spec.query.fast_reject_filename(name) {
                continue;
            }

            // Verify parent reachability & depth under current `tree_gen`.
            let parent_dir_idx = if edge.parent_ref == self.root_ref {
                match self.ref_to_obj.get(&self.root_ref) {
                    Some(&idx) => idx as usize,
                    None => continue,
                }
            } else {
                match self.ref_to_obj.get(&edge.parent_ref) {
                    Some(&idx) => idx as usize,
                    None => continue,
                }
            };
            let Some(parent_depth) = self
                .reachability
                .dir_depth
                .get(parent_dir_idx)
                .copied()
                .flatten()
            else {
                continue;
            };
            let entry_depth = parent_depth.saturating_add(1);
            if entry_depth <= subtree_depth_base {
                continue;
            }
            if let Some(max_d) = spec.max_depth
                && usize::from(entry_depth - subtree_depth_base) > max_d
            {
                continue;
            }

            let Some(parent_rel) = self.dir_rel_path(parent_dir_idx) else {
                continue;
            };
            let full_rel = if parent_rel.is_empty() {
                name.to_owned()
            } else {
                format!("{parent_rel}/{name}")
            };

            let rel_for_root = if let Some(ref prefix_lower) = subtree_prefix_lower {
                let full_lower = full_rel.to_ascii_lowercase();
                if !full_lower.starts_with(prefix_lower.as_str()) {
                    continue;
                }
                &full_rel[prefix_lower.len()..]
            } else {
                full_rel.as_str()
            };

            if !spec.excludes.is_empty()
                && (spec.excludes.is_match(rel_for_root)
                    || spec.excludes.is_match(name)
                    || ancestor_dir_excluded(rel_for_root, &spec.excludes))
            {
                continue;
            }

            let Some(score) = spec.query.score_entry(rel_for_root, name, &mut scratch) else {
                continue;
            };

            let abs_display = format!("{}/{}", self.drive_upper, full_rel);
            let probe = FindCandidate {
                score,
                path: abs_display.clone(),
                is_dir,
                size_bytes: (!is_dir).then_some(obj.size_bytes),
                modified_secs: (obj.mtime_secs != 0).then_some(obj.mtime_secs),
                modified_iso: None,
            };
            if local.len() >= cap
                && !meta_stale
                && local
                    .last()
                    .is_some_and(|worst| !cmp_find_candidate(&probe, worst, sort_by).is_lt())
            {
                continue;
            }

            // P0-3: Verify caller holds FILE_LIST_DIRECTORY permission on EVERY ancestor directory
            // between `verified_root` and this candidate's parent so raw $MFT never leaks entries
            // hidden by subdirectory ACLs.
            #[cfg(windows)]
            if !self.verify_ancestor_list_permissions(
                session,
                verified_root,
                parent_dir_idx,
                subtree_depth_base,
                &mut dir_list_cache,
            ) {
                continue;
            }
            #[cfg(not(windows))]
            let _ = &mut dir_list_cache;

            let native_path = PathBuf::from(format!(
                "{}\\{}",
                self.drive_upper,
                full_rel.replace('/', "\\")
            ));
            let verified = match session.verify_search_entry(
                &native_path,
                verified_root,
                Some(self.identity.serial),
                Some(obj.file_ref),
                is_dir,
            ) {
                Ok(v) => v,
                Err(SearchGuardError::PolicyDenied) => continue,
                Err(SearchGuardError::IoOrAccess) => {
                    skipped += 1;
                    continue;
                }
            };
            if verified.is_dir != is_dir {
                continue;
            }
            if !path_within_modified_window(verified.modified_secs, spec.modified_window) {
                continue;
            }
            if !verified.is_dir && !size_within_bounds(verified.size_bytes, spec.size_bounds) {
                continue;
            }

            let candidate = FindCandidate {
                score,
                path: normalize_path(&verified.canonical_path),
                is_dir: verified.is_dir,
                size_bytes: verified.size_bytes,
                modified_secs: verified.modified_secs,
                modified_iso: verified.modified_secs.and_then(format_local_rfc3339),
            };
            if local.len() < cap {
                let pos = local
                    .binary_search_by(|existing| cmp_find_candidate(existing, &candidate, sort_by))
                    .unwrap_or_else(|i| i);
                local.insert(pos, candidate);
                if local.len() == cap {
                    truncated_by_limit = true;
                }
            } else if let Some(worst) = local.last()
                && cmp_find_candidate(&candidate, worst, sort_by).is_lt()
            {
                local.pop();
                let pos = local
                    .binary_search_by(|existing| cmp_find_candidate(existing, &candidate, sort_by))
                    .unwrap_or_else(|i| i);
                local.insert(pos, candidate);
            }
        }

        Ok((local, truncated_by_limit, skipped, timed_out))
    }

    #[cfg(windows)]
    fn verify_ancestor_list_permissions(
        &self,
        session: &PathGuardSession,
        verified_root: &VerifiedSearchEntry,
        parent_dir_idx: usize,
        subtree_depth_base: u16,
        cache: &mut HashMap<u32, bool>,
    ) -> bool {
        let mut chain: Vec<usize> = Vec::new();
        let mut cur = parent_dir_idx;
        while let Some(depth) = self.reachability.dir_depth.get(cur).copied().flatten() {
            if depth <= subtree_depth_base {
                break;
            }
            if let Some(&ok) = cache.get(&(cur as u32)) {
                if !ok {
                    for &d in &chain {
                        cache.insert(d as u32, false);
                    }
                    return false;
                }
                break;
            }
            chain.push(cur);
            let Some(edge_idx) = self
                .reachability
                .dir_parent_edge
                .get(cur)
                .copied()
                .flatten()
            else {
                return false;
            };
            let parent_ref = self.edges[edge_idx as usize].parent_ref;
            if parent_ref == self.root_ref {
                break;
            }
            let Some(&next_idx) = self.ref_to_obj.get(&parent_ref) else {
                return false;
            };
            cur = next_idx as usize;
        }
        // Verify top-down so denied outer directory immediately prunes inner ancestors.
        for &dir_idx in chain.iter().rev() {
            let Some(rel) = self.dir_rel_path(dir_idx) else {
                cache.insert(dir_idx as u32, false);
                return false;
            };
            let native = PathBuf::from(format!("{}\\{}", self.drive_upper, rel.replace('/', "\\")));
            let expected_ref = self.objects[dir_idx].file_ref;
            let ok = session
                .verify_dir_list_identity(&native, verified_root, expected_ref)
                .is_ok();
            cache.insert(dir_idx as u32, ok);
            if !ok {
                return false;
            }
        }
        true
    }
}

fn ancestor_dir_excluded(rel_path: &str, excludes: &globset::GlobSet) -> bool {
    let mut pos = 0usize;
    while let Some(slash) = rel_path[pos..].find('/') {
        let end = pos + slash;
        let prefix = &rel_path[..end];
        let name = &rel_path[pos..end];
        let with_slash = format!("{prefix}/");
        if excludes.is_match(prefix)
            || excludes.is_match(&with_slash)
            || excludes.is_match(name)
            || excludes.is_match(format!("{name}/"))
        {
            return true;
        }
        pos = end + 1;
    }
    false
}

// ============================================================================
// Custom ntfs-ng MFT Projection (extracts ALL files, dirs, mtime, and hard links)
// ============================================================================

#[derive(Clone, Debug)]
struct ProjectedObject {
    file_ref: u64,
    is_dir: bool,
    is_reparse: bool,
    is_cloud_hydrated: bool,
    size_bytes: u64,
    mtime_secs: i64,
    names: Vec<(u64, u8, u32, u16)>, // (parent_ref, namespace, name_start, name_len)
}

#[derive(Clone, Debug)]
struct ProjectedExtension {
    base_ref: u64,
    size_bytes: Option<u64>,
    reparse_tag: Option<u32>,
    names: Vec<(u64, u8, u32, u16)>,
}

#[derive(Default)]
struct SearchChunkOut {
    objects: Vec<ProjectedObject>,
    extensions: Vec<ProjectedExtension>,
    names: String,
    attr_errors: u64,
}

impl SearchChunkOut {
    fn est_bytes(&self) -> usize {
        self.objects.len() * 64 + self.extensions.len() * 48 + self.names.len()
    }
}

struct SearchMftProjection;

fn extract_mft_data_size(attr: &ntfs_ng::Attribute<'_>) -> Option<u64> {
    if attr.is_resident() {
        attr.data_size().ok()
    } else {
        let ext = attr.non_resident().ok()?;
        ext.is_first().then_some(ext.data_size)
    }
}

fn extract_mft_reparse_tag(attr: &ntfs_ng::Attribute<'_>) -> Option<u32> {
    let val = attr.value().ok()?;
    val.first_chunk::<4>().map(|c| u32::from_le_bytes(*c))
}

fn ntfs_filetime_to_unix_i64(ft: u64) -> i64 {
    const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
    if ft == 0 {
        return 0;
    }
    let delta = ft as i128 - EPOCH_DIFF as i128;
    delta.div_euclid(10_000_000) as i64
}

impl Projection for SearchMftProjection {
    type Output = SearchChunkOut;
    type Scratch = Vec<(u64, u8, bool, u32, u16)>;

    fn scratch(&self) -> Self::Scratch {
        Vec::with_capacity(4)
    }

    fn begin(&self, _scratch: &mut Self::Scratch, _chunk: Chunk) -> SearchChunkOut {
        SearchChunkOut::default()
    }

    fn entry(
        &self,
        out: &mut SearchChunkOut,
        names_scratch: &mut Self::Scratch,
        number: u64,
        entry: ScanEntry<'_>,
    ) {
        let ScanEntry::Record(record) = entry else {
            return;
        };
        let header = record.header();
        if !header.in_use() {
            return;
        }
        if !header.is_base() {
            let mut ext = ProjectedExtension {
                base_ref: header.base_record.to_u64(),
                size_bytes: None,
                reparse_tag: None,
                names: Vec::new(),
            };
            for attr in record.attributes() {
                let Ok(attr) = attr else {
                    out.attr_errors += 1;
                    break;
                };
                match attr.type_code() {
                    types::DATA if attr.is_unnamed() => {
                        if let Some(s) = extract_mft_data_size(&attr) {
                            ext.size_bytes = Some(s);
                        }
                    }
                    types::REPARSE_POINT => {
                        ext.reparse_tag = Some(extract_mft_reparse_tag(&attr).unwrap_or(0));
                    }
                    types::FILE_NAME => {
                        if let Ok(value) = attr.value()
                            && let Ok((parent, ns, name)) = FileName::link(value)
                            && !ns.is_dos_only()
                        {
                            let start = out.names.len();
                            name.push_lossy(&mut out.names);
                            if let (Ok(s32), Ok(l16)) =
                                (u32::try_from(start), u16::try_from(out.names.len() - start))
                            {
                                ext.names.push((parent.to_u64(), ns as u8, s32, l16));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if ext.size_bytes.is_some() || ext.reparse_tag.is_some() || !ext.names.is_empty() {
                out.extensions.push(ext);
            }
            return;
        }

        let is_root = number == records::ROOT;
        let is_user = number >= records::FIRST_USER;
        if !is_root && !is_user {
            return;
        }

        let file_ref = FileReference::new(number, header.sequence).to_u64();
        let is_dir = header.is_directory();
        let mut size_bytes = 0u64;
        let mut mtime_secs = 0i64;
        let mut reparse_tag: Option<u32> = None;
        names_scratch.clear();

        for attr in record.attributes() {
            let Ok(attr) = attr else {
                out.attr_errors += 1;
                break;
            };
            match attr.type_code() {
                types::STANDARD_INFORMATION => {
                    if let Ok(value) = attr.value()
                        && let Some(times) = StandardInformation::times_of(value)
                    {
                        mtime_secs = ntfs_filetime_to_unix_i64(times.modified);
                    }
                }
                types::FILE_NAME => {
                    if let Ok(value) = attr.value()
                        && let Ok(fn_val) = FileName::parse(value)
                    {
                        let start = out.names.len();
                        fn_val.name.push_lossy(&mut out.names);
                        if let (Ok(s32), Ok(l16)) =
                            (u32::try_from(start), u16::try_from(out.names.len() - start))
                        {
                            names_scratch.push((
                                fn_val.parent.to_u64(),
                                fn_val.namespace as u8,
                                fn_val.namespace.is_dos_only(),
                                s32,
                                l16,
                            ));
                        }
                    }
                }
                types::DATA if !is_dir && attr.is_unnamed() => {
                    if let Some(s) = extract_mft_data_size(&attr) {
                        size_bytes = s;
                    }
                }
                types::REPARSE_POINT => {
                    reparse_tag = Some(extract_mft_reparse_tag(&attr).unwrap_or(0));
                }
                _ => {}
            }
        }

        // Keep all real hard links while dropping pure DOS 8.3 aliases when a Win32/POSIX name
        // exists in the same parent directory.
        let mut kept_names = Vec::with_capacity(names_scratch.len());
        for &(parent, ns, dos_only, start, len) in names_scratch.iter() {
            if dos_only
                && names_scratch
                    .iter()
                    .any(|&(p2, _, d2, ..)| p2 == parent && !d2)
            {
                continue;
            }
            kept_names.push((parent, ns, start, len));
        }

        let is_reparse = reparse_tag.is_some();
        let is_cloud_hydrated = reparse_tag.is_some_and(is_cloud_reparse_tag);
        out.objects.push(ProjectedObject {
            file_ref,
            is_dir,
            is_reparse,
            is_cloud_hydrated,
            size_bytes,
            mtime_secs,
            names: kept_names,
        });
    }
}

pub(crate) fn build_index_from_mft_source<S: ReadAt + Sync>(
    drive_upper: &str,
    identity: VolumeIdentity,
    journal_id: u64,
    u0_cursor: i64,
    mft: &ntfs_ng::mft::Mft<S>,
    ledger: &Arc<ProcessMemoryLedger>,
    cancel: &CancellationToken,
) -> Result<VolumeFileIndex, String> {
    let total_records = mft.records();
    if total_records == 0 || total_records > MAX_MFT_SEARCH_RECORDS {
        return Err("MFT 记录数超限，回退目录遍历。".into());
    }

    let cancel_flag = AtomicBool::new(false);
    let mem_exceeded = AtomicBool::new(false);
    let mut chunks: Vec<SearchChunkOut> = Vec::new();
    let mut mem_bytes = 0usize;

    let threads = super::worker_threads();
    let summary = Scan::new(mft)
        .threads(threads)
        .chunk_bytes(4 << 20)
        .cancel(&cancel_flag)
        .ordered(false)
        .run(&SearchMftProjection, |_chunk, out| {
            if cancel.is_cancelled() {
                cancel_flag.store(true, Ordering::Release);
                return std::ops::ControlFlow::Break(());
            }
            mem_bytes = mem_bytes.saturating_add(out.est_bytes());
            if mem_bytes > MAX_VOLUME_INDEX_BYTES {
                mem_exceeded.store(true, Ordering::Release);
                cancel_flag.store(true, Ordering::Release);
                return std::ops::ControlFlow::Break(());
            }
            if !out.objects.is_empty() || !out.extensions.is_empty() || out.attr_errors > 0 {
                chunks.push(out);
            }
            std::ops::ControlFlow::Continue(())
        })
        .map_err(|e| format!("读取 $MFT 失败：{e}"))?;

    if cancel.is_cancelled() {
        return Err("文件检索已取消。".into());
    }
    if mem_exceeded.load(Ordering::Acquire) {
        return Err("卷搜索索引超过单卷内存上限，回退目录遍历。".into());
    }
    if summary.unreadable > 0
        || summary.unmapped > 0
        || summary.damaged > 0
        || summary.stopped
        || chunks.iter().any(|c| c.attr_errors > 0)
    {
        return Err("$MFT 完整性校验未通过，回退目录遍历。".into());
    }

    let obj_total: usize = chunks.iter().map(|c| c.objects.len()).sum();
    let mut objects = Vec::with_capacity(obj_total);
    let mut edges = Vec::with_capacity(obj_total);
    let mut name_arena = Vec::with_capacity(obj_total.saturating_mul(16));
    let mut ref_to_obj: HashMap<u64, u32> = HashMap::with_capacity(obj_total);
    let mut root_ref = FileReference::new(records::ROOT, 5).to_u64();

    // First pass: materialize base objects and their names.
    for chunk in &chunks {
        for proj in &chunk.objects {
            if FileReference::from_u64(proj.file_ref).record == records::ROOT {
                root_ref = proj.file_ref;
            }
            let obj_idx = objects.len() as u32;
            let first_edge = if proj.names.is_empty() {
                u32::MAX
            } else {
                edges.len() as u32
            };
            let mut flags = 0u16;
            if proj.is_dir {
                flags |= FLAG_IS_DIR;
            }
            if proj.is_reparse {
                flags |= FLAG_IS_REPARSE;
            }
            if proj.is_cloud_hydrated {
                flags |= FLAG_IS_CLOUD_HYDRATED;
            }
            let mut count = 0u16;
            for &(parent_ref, ns, start, len) in &proj.names {
                let s = start as usize;
                let e = s + len as usize;
                let name_slice = &chunk.names.as_bytes()[s..e];
                let Ok(off) = u32::try_from(name_arena.len()) else {
                    return Err("名称池超限，回退目录遍历。".into());
                };
                name_arena.extend_from_slice(name_slice);
                let flags = if name_slice.contains(&0xEF)
                    && std::str::from_utf8(name_slice)
                        .is_ok_and(|s| s.contains(char::REPLACEMENT_CHARACTER))
                {
                    EDGE_FLAG_LOSSY_NAME
                } else {
                    0
                };
                edges.push(NameEdge {
                    parent_ref,
                    obj_idx,
                    name_offset: off,
                    name_len: len,
                    namespace: ns,
                    flags,
                });
                count = count.saturating_add(1);
            }
            objects.push(ObjectNode {
                file_ref: proj.file_ref,
                size_bytes: proj.size_bytes,
                mtime_secs: proj.mtime_secs,
                flags,
                edge_count: count,
                first_edge,
            });
            ref_to_obj.insert(proj.file_ref, obj_idx);
        }
    }

    // Second pass: merge extension records ($ATTRIBUTE_LIST).
    for chunk in &chunks {
        for ext in &chunk.extensions {
            let Some(&obj_idx) = ref_to_obj.get(&ext.base_ref) else {
                continue;
            };
            let oi = obj_idx as usize;
            if let Some(sz) = ext.size_bytes
                && objects[oi].size_bytes == 0
            {
                objects[oi].size_bytes = sz;
            }
            if let Some(tag) = ext.reparse_tag {
                objects[oi].flags |= FLAG_IS_REPARSE;
                if is_cloud_reparse_tag(tag) {
                    objects[oi].flags |= FLAG_IS_CLOUD_HYDRATED;
                }
            }
            for &(parent_ref, ns, start, len) in &ext.names {
                let s = start as usize;
                let e = s + len as usize;
                let name_slice = &chunk.names.as_bytes()[s..e];
                let Ok(off) = u32::try_from(name_arena.len()) else {
                    return Err("名称池超限，回退目录遍历。".into());
                };
                name_arena.extend_from_slice(name_slice);
                let edge_idx = edges.len() as u32;
                let flags = if name_slice.contains(&0xEF)
                    && std::str::from_utf8(name_slice)
                        .is_ok_and(|s| s.contains(char::REPLACEMENT_CHARACTER))
                {
                    EDGE_FLAG_LOSSY_NAME
                } else {
                    0
                };
                edges.push(NameEdge {
                    parent_ref,
                    obj_idx,
                    name_offset: off,
                    name_len: len,
                    namespace: ns,
                    flags,
                });
                if objects[oi].first_edge == u32::MAX {
                    objects[oi].first_edge = edge_idx;
                }
                objects[oi].edge_count = objects[oi].edge_count.saturating_add(1);
            }
        }
    }

    let estimated_bytes =
        VolumeFileIndex::compute_estimated_bytes(&objects, &edges, &name_arena, &ref_to_obj);
    if estimated_bytes > MAX_VOLUME_INDEX_BYTES {
        return Err("卷搜索索引超过单卷内存上限，回退目录遍历。".into());
    }
    let Some(mem_guard) = ledger.try_reserve(estimated_bytes) else {
        return Err("进程级缓存预算不足，回退目录遍历。".into());
    };

    let live_objects = objects.len();
    let mut index = VolumeFileIndex {
        drive_upper: drive_upper.to_ascii_uppercase(),
        identity,
        journal_id,
        applied_cursor: u0_cursor,
        root_ref,
        objects,
        edges,
        name_arena,
        ref_to_obj,
        live_objects,
        tombstones: 0,
        reachability: ReachabilityState {
            tree_gen: 0,
            dir_depth: Vec::new(),
            dir_parent_edge: Vec::new(),
        },
        estimated_bytes,
        mem_guard: Some(mem_guard),
    };
    index.recompute_reachability();
    Ok(index)
}

// ============================================================================
// Windows USN Journal V2 Raw IOCTL + Buffer Parser
// ============================================================================

#[derive(Clone, Copy, Debug)]
pub(crate) enum UsnReadError {
    NotActive,
    Wrapped,
    UnsupportedVersion,
    AccessDenied,
    IoFailed,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UsnJournalCursor {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

/// Safe zero-UB parser for a `FSCTL_READ_USN_JOURNAL` response buffer.
/// First 8 bytes are the continuation `next_usn`; subsequent records must be V2
/// and fill the returned buffer completely without trailing truncated fragments.
pub(crate) fn parse_usn_read_buffer(buf: &[u8]) -> Result<(i64, Vec<UsnEventV2>), UsnReadError> {
    if buf.len() < 8 {
        return Err(UsnReadError::IoFailed);
    }
    let next_usn = i64::from_le_bytes(buf[0..8].try_into().unwrap());
    let mut events = Vec::new();
    let mut off = 8usize;
    while off < buf.len() {
        if off + 60 > buf.len() {
            // Never silently accept truncated trailing bytes.
            return Err(UsnReadError::IoFailed);
        }
        let rec_len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        if rec_len < 60 || off + rec_len > buf.len() {
            return Err(UsnReadError::IoFailed);
        }
        let major = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap());
        if major != 2 {
            // Never truncate V3 128-bit IDs; cleanly fall back.
            return Err(UsnReadError::UnsupportedVersion);
        }
        let file_ref = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap());
        let parent_ref = u64::from_le_bytes(buf[off + 16..off + 24].try_into().unwrap());
        let usn = i64::from_le_bytes(buf[off + 24..off + 32].try_into().unwrap());
        let reason = u32::from_le_bytes(buf[off + 40..off + 44].try_into().unwrap());
        let file_attributes = u32::from_le_bytes(buf[off + 52..off + 56].try_into().unwrap());
        let name_len_bytes =
            u16::from_le_bytes(buf[off + 56..off + 58].try_into().unwrap()) as usize;
        let name_offset = u16::from_le_bytes(buf[off + 58..off + 60].try_into().unwrap()) as usize;
        if name_offset < 60
            || name_offset + name_len_bytes > rec_len
            || !name_len_bytes.is_multiple_of(2)
        {
            return Err(UsnReadError::IoFailed);
        }
        let name_slice = &buf[off + name_offset..off + name_offset + name_len_bytes];
        let mut name_utf8 = String::with_capacity(name_len_bytes / 2);
        let mut lossless = true;
        let units = name_slice
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c));
        for ch in char::decode_utf16(units) {
            match ch {
                Ok(c) => name_utf8.push(c),
                Err(_) => {
                    lossless = false;
                    name_utf8.push(char::REPLACEMENT_CHARACTER);
                }
            }
        }
        events.push(UsnEventV2 {
            file_ref,
            parent_ref,
            usn,
            reason,
            file_attributes,
            name_utf8,
            name_lossless: lossless,
        });
        off += rec_len;
    }
    Ok((next_usn, events))
}

#[cfg(windows)]
mod win_usn {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_JOURNAL_DELETE_IN_PROGRESS,
        ERROR_JOURNAL_ENTRY_DELETED, ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL};
    use windows::core::PCWSTR;

    #[repr(C)]
    #[derive(Default)]
    struct UsnJournalDataV0 {
        usn_journal_id: u64,
        first_usn: i64,
        next_usn: i64,
        lowest_valid_usn: i64,
        max_usn: i64,
        maximum_size: u64,
        allocation_delta: u64,
    }

    #[repr(C)]
    struct ReadUsnJournalDataV0 {
        start_usn: i64,
        reason_mask: u32,
        return_only_on_close: u32,
        timeout: u64,
        bytes_to_wait_for: u64,
        usn_journal_id: u64,
    }

    pub(super) struct VolumeUsnHandle(HANDLE);
    impl Drop for VolumeUsnHandle {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    fn map_win32_usn_err(err: &windows::core::Error) -> UsnReadError {
        let code = err.code().0 as u32 & 0xFFFF;
        if code == ERROR_JOURNAL_NOT_ACTIVE.0 || code == ERROR_JOURNAL_DELETE_IN_PROGRESS.0 {
            UsnReadError::NotActive
        } else if code == ERROR_JOURNAL_ENTRY_DELETED.0 {
            UsnReadError::Wrapped
        } else if code == ERROR_ACCESS_DENIED.0 {
            UsnReadError::AccessDenied
        } else {
            UsnReadError::IoFailed
        }
    }

    pub(super) fn open_volume_read_handle(
        raw_vol_path: &str,
    ) -> Result<VolumeUsnHandle, UsnReadError> {
        let wide: Vec<u16> = OsStr::new(raw_vol_path)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(|e| map_win32_usn_err(&e))?;
        if h == INVALID_HANDLE_VALUE {
            return Err(UsnReadError::IoFailed);
        }
        Ok(VolumeUsnHandle(h))
    }

    pub(super) fn query_usn_cursor(
        handle: &VolumeUsnHandle,
    ) -> Result<UsnJournalCursor, UsnReadError> {
        let mut data = UsnJournalDataV0::default();
        let mut ret = 0u32;
        unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some((&raw mut data).cast()),
                std::mem::size_of::<UsnJournalDataV0>() as u32,
                Some(&mut ret),
                None,
            )
        }
        .map_err(|e| map_win32_usn_err(&e))?;
        if (ret as usize) < std::mem::size_of::<UsnJournalDataV0>() {
            return Err(UsnReadError::IoFailed);
        }
        Ok(UsnJournalCursor {
            journal_id: data.usn_journal_id,
            first_usn: data.first_usn,
            next_usn: data.next_usn,
            lowest_valid_usn: data.lowest_valid_usn,
        })
    }

    /// Reads all USN V2 events from `start_usn` up to `high_water_usn` (capped by `max_events`
    /// so runaway journal backlogs cleanly fall back to rebuild).
    /// Fails with `Wrapped` if the journal cursor stalls or regresses before `high_water_usn`.
    pub(super) fn read_usn_range(
        handle: &VolumeUsnHandle,
        journal_info: UsnJournalCursor,
        start_usn: i64,
        high_water_usn: i64,
        max_events: usize,
        cancel: &CancellationToken,
    ) -> Result<(i64, Vec<UsnEventV2>), UsnReadError> {
        if start_usn < journal_info.first_usn || start_usn < journal_info.lowest_valid_usn {
            return Err(UsnReadError::Wrapped);
        }
        let mut cursor = start_usn;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];

        while cursor < high_water_usn {
            if cancel.is_cancelled() {
                return Err(UsnReadError::IoFailed);
            }
            let req = ReadUsnJournalDataV0 {
                start_usn: cursor,
                reason_mask: 0xFFFF_FFFF,
                return_only_on_close: 0,
                timeout: 0,
                bytes_to_wait_for: 0,
                usn_journal_id: journal_info.journal_id,
            };
            let mut ret = 0u32;
            unsafe {
                DeviceIoControl(
                    handle.0,
                    FSCTL_READ_USN_JOURNAL,
                    Some((&raw const req).cast()),
                    std::mem::size_of::<ReadUsnJournalDataV0>() as u32,
                    Some(buf.as_mut_ptr().cast()),
                    buf.len() as u32,
                    Some(&mut ret),
                    None,
                )
            }
            .map_err(|e| map_win32_usn_err(&e))?;

            let (next_cursor, batch) = parse_usn_read_buffer(&buf[..ret as usize])?;
            if next_cursor < cursor {
                return Err(UsnReadError::Wrapped);
            }
            if batch.is_empty() {
                if next_cursor >= high_water_usn {
                    cursor = next_cursor;
                    break;
                }
                return Err(UsnReadError::Wrapped);
            }
            for ev in batch {
                if ev.usn >= high_water_usn {
                    return Ok((ev.usn, out));
                }
                out.push(ev);
                if out.len() > max_events {
                    return Err(UsnReadError::Wrapped);
                }
            }
            if next_cursor <= cursor {
                return Err(UsnReadError::Wrapped);
            }
            cursor = next_cursor;
        }
        if cursor < high_water_usn {
            return Err(UsnReadError::Wrapped);
        }
        Ok((cursor, out))
    }

    pub(super) fn cold_build_windows_volume(
        drive_upper: &str,
        identity: &VolumeIdentity,
        ledger: &Arc<ProcessMemoryLedger>,
        cancel: &CancellationToken,
    ) -> Result<VolumeFileIndex, String> {
        let raw_path = identity
            .volume_guid
            .as_deref()
            .map(|g| g.trim_end_matches('\\').to_owned())
            .filter(|g| g.starts_with(r"\\?\Volume{"))
            .unwrap_or_else(|| format!(r"\\.\{drive_upper}"));

        // Step 1 (P0-1 barrier): bind volume handle and read `(JournalId, U0)` BEFORE scanning $MFT.
        let usn_handle = open_volume_read_handle(&raw_path)
            .map_err(|e| format!("无法打开只读卷句柄查询 USN：{e:?}"))?;
        let u0 = query_usn_cursor(&usn_handle)
            .map_err(|e| format!("目标卷未启用 USN 或无法读取游标：{e:?}"))?;

        // Step 2: scan $MFT with `ntfs-ng`.
        let device = ntfs_ng::source::Device::open(
            &raw_path,
            ntfs_ng::source::DeviceOptions {
                unbuffered: true,
                alignment: 4096,
                len: None,
            },
        )
        .map_err(|e| format!("无法以只读方式打开卷 {drive_upper}：{e}"))?;
        let mut boot_buf = [0u8; 512];
        ntfs_ng::source::read_at(&device, 0, &mut boot_buf)
            .map_err(|e| format!("读取引导扇区失败：{e}"))?;
        let boot = ntfs_ng::BootSector::parse(&boot_buf)
            .map_err(|e| format!("NTFS 引导扇区无效：{e:?}"))?;
        let mft = ntfs_ng::mft::Mft::from_volume(&device, &boot)
            .map_err(|e| format!("解析 $MFT 失败：{e:?}"))?;

        let mut index = build_index_from_mft_source(
            drive_upper,
            identity.clone(),
            u0.journal_id,
            u0.next_usn,
            &mft,
            ledger,
            cancel,
        )?;

        // Step 3: read high-water mark `U1` and replay `[U0, U1)`.
        let u1 = query_usn_cursor(&usn_handle)
            .map_err(|e| format!("冷建后读取 USN 高水位失败：{e:?}"))?;
        if u1.journal_id != u0.journal_id {
            return Err("冷建期间 USN 日志代际变化，放弃本次索引。".into());
        }
        if u1.next_usn > u0.next_usn {
            let (applied, events) =
                read_usn_range(&usn_handle, u1, u0.next_usn, u1.next_usn, 200_000, cancel)
                    .map_err(|e| format!("冷建窗口 USN 重放失败：{e:?}"))?;
            if applied < u1.next_usn {
                return Err("冷建窗口 USN 未覆盖完整高水位，放弃本次索引。".into());
            }
            index
                .apply_usn_events(&events, applied)
                .map_err(|()| "冷建窗口 USN 应用超限，放弃本次索引。".to_owned())?;
        }

        // Verify volume identity did not change during cold build.
        let latest_id = crate::diagnostics::volume::volume_identity(drive_upper)?;
        if latest_id != *identity {
            return Err("冷建期间卷身份变化，放弃本次索引。".into());
        }
        Ok(index)
    }

    pub(super) fn sync_windows_volume(
        current: &VolumeFileIndex,
        ledger: &Arc<ProcessMemoryLedger>,
        cancel: &CancellationToken,
    ) -> Result<Option<VolumeFileIndex>, UsnReadError> {
        let raw_path = current
            .identity
            .volume_guid
            .as_deref()
            .map(|g| g.trim_end_matches('\\').to_owned())
            .filter(|g| g.starts_with(r"\\?\Volume{"))
            .unwrap_or_else(|| format!(r"\\.\{}", current.drive_upper));
        let usn_handle = open_volume_read_handle(&raw_path)?;
        let cur = query_usn_cursor(&usn_handle)?;
        if cur.journal_id != current.journal_id {
            return Err(UsnReadError::Wrapped);
        }
        if cur.next_usn <= current.applied_cursor {
            return Ok(None);
        }
        let (applied, events) = read_usn_range(
            &usn_handle,
            cur,
            current.applied_cursor,
            cur.next_usn,
            100_000,
            cancel,
        )?;
        if events.is_empty() && applied == current.applied_cursor {
            return Ok(None);
        }
        let mut next = current
            .clone_with_reservation(ledger)
            .ok_or(UsnReadError::Wrapped)?;
        next.apply_usn_events(&events, applied)
            .map_err(|()| UsnReadError::Wrapped)?;
        Ok(Some(next))
    }
}

// ============================================================================
// Process-Wide SearchIndexManager (Singleflight, 384 MiB Ledger, 180s Idle TTL)
// ============================================================================

#[derive(Clone)]
pub(crate) enum IndexBuildStatus {
    ColdBuilt,
    UsnSynced,
}

struct CachedVolumeEntry {
    index: Arc<VolumeFileIndex>,
    last_accessed: Instant,
}

#[derive(Clone)]
enum BuildOutcome {
    Ready(Arc<VolumeFileIndex>, IndexBuildStatus),
    Fallback,
    Cancelled,
}

struct BuildFlight {
    build_cancel: CancellationToken,
    subscribers: AtomicUsize,
    result: Mutex<Option<BuildOutcome>>,
    ready: Condvar,
}

impl BuildFlight {
    fn new() -> Self {
        Self {
            build_cancel: CancellationToken::new(),
            subscribers: AtomicUsize::new(0),
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn finish(&self, outcome: BuildOutcome) {
        if let Ok(mut slot) = self.result.lock() {
            *slot = Some(outcome);
        }
        self.ready.notify_all();
    }

    fn wait(&self, deadline: Instant, cancel: &CancellationToken) -> BuildOutcome {
        let mut guard = self.result.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(outcome) = guard.as_ref() {
                return outcome.clone();
            }
            if cancel.is_cancelled() {
                return BuildOutcome::Cancelled;
            }
            if Instant::now() >= deadline {
                return BuildOutcome::Fallback;
            }
            let (next, _) = self
                .ready
                .wait_timeout(guard, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner());
            guard = next;
        }
    }
}

struct FlightSub {
    manager: Arc<SearchIndexManager>,
    key: (String, VolumeIdentity),
    flight: Arc<BuildFlight>,
}

impl Drop for FlightSub {
    fn drop(&mut self) {
        if self.flight.subscribers.fetch_sub(1, Ordering::AcqRel) == 1
            && let Ok(mut state) = self.manager.state.lock()
            && self.flight.subscribers.load(Ordering::Acquire) == 0
        {
            let done = self.flight.result.lock().is_ok_and(|g| g.is_some());
            if !done {
                self.flight.build_cancel.cancel();
                if state
                    .pending
                    .get(&self.key)
                    .is_some_and(|active| Arc::ptr_eq(active, &self.flight))
                {
                    state.pending.remove(&self.key);
                }
            }
        }
    }
}

#[derive(Default)]
struct ManagerState {
    volumes: HashMap<String, CachedVolumeEntry>,
    pending: HashMap<(String, VolumeIdentity), Arc<BuildFlight>>,
    /// Drives where USN was observed unavailable; short cooldown to avoid repeated IOCTL failures.
    usn_unavailable: HashMap<String, Instant>,
    /// Generation counter for monotonic invalidation.
    gen_seq: u64,
}

pub(crate) struct SearchIndexManager {
    state: Mutex<ManagerState>,
    active_builds: AtomicUsize,
    idle_ttl: Duration,
    ledger: Arc<ProcessMemoryLedger>,
    reaper_started: AtomicBool,
}

impl SearchIndexManager {
    #[cfg(test)]
    pub(crate) fn new(idle_ttl: Duration, max_process_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ManagerState::default()),
            active_builds: AtomicUsize::new(0),
            idle_ttl,
            ledger: ProcessMemoryLedger::new_isolated(max_process_bytes, max_process_bytes),
            reaper_started: AtomicBool::new(false),
        })
    }

    pub(crate) fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<SearchIndexManager>> = OnceLock::new();
        let mgr = Arc::clone(GLOBAL.get_or_init(|| {
            Arc::new(Self {
                state: Mutex::new(ManagerState::default()),
                active_builds: AtomicUsize::new(0),
                idle_ttl: IDLE_TTL,
                ledger: ProcessMemoryLedger::global(),
                reaper_started: AtomicBool::new(false),
            })
        }));
        mgr.ensure_reaper_started();
        mgr
    }

    fn ensure_reaper_started(self: &Arc<Self>) {
        if !self.reaper_started.swap(true, Ordering::AcqRel) {
            let weak = Arc::downgrade(self);
            let res = std::thread::Builder::new()
                .name("cst-fff-index-reaper".into())
                .spawn(move || {
                    while let Some(mgr) = weak.upgrade() {
                        std::thread::sleep(Duration::from_secs(15));
                        mgr.purge_idle();
                    }
                });
            if res.is_err() {
                // Retry on next request if thread creation failed transiently.
                self.reaper_started.store(false, Ordering::Release);
            }
        }
    }

    pub(crate) fn purge_idle(&self) {
        if let Ok(mut state) = self.state.lock() {
            let ttl = self.idle_ttl;
            state
                .volumes
                .retain(|_, entry| entry.last_accessed.elapsed() < ttl);
            state
                .usn_unavailable
                .retain(|_, at| at.elapsed() < Duration::from_secs(60));
        }
    }

    #[cfg(test)]
    pub(crate) fn cached_bytes(&self) -> usize {
        self.state
            .lock()
            .map(|s| s.volumes.values().map(|v| v.index.estimated_bytes).sum())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn reserved_bytes(&self) -> usize {
        self.ledger.reserved_bytes()
    }

    #[cfg(test)]
    pub(crate) fn insert_for_test(
        &self,
        mut index: VolumeFileIndex,
        accessed_at: Instant,
    ) -> Option<Arc<VolumeFileIndex>> {
        let mut state = self.state.lock().ok()?;
        let need = index.estimated_bytes;
        // Evict oldest cached entries until ledger reservation succeeds (evicted entries still
        // held by live Arc readers keep their bytes charged until Drop).
        let guard = loop {
            if let Some(g) = self.ledger.try_reserve(need) {
                break g;
            }
            let oldest = state
                .volumes
                .iter()
                .min_by_key(|(_, e)| e.last_accessed)
                .map(|(k, _)| k.clone())?;
            state.volumes.remove(&oldest);
        };
        index.mem_guard = Some(guard);
        let arc = Arc::new(index);
        state.volumes.insert(
            arc.drive_upper.clone(),
            CachedVolumeEntry {
                index: Arc::clone(&arc),
                last_accessed: accessed_at,
            },
        );
        self.evict_lru_locked(&mut state);
        Some(arc)
    }

    fn evict_lru_locked(&self, state: &mut ManagerState) {
        let search_cap = self.ledger.search_cap();
        loop {
            let total_search: usize = state
                .volumes
                .values()
                .map(|v| v.index.estimated_bytes)
                .sum();
            if total_search <= search_cap && self.ledger.reserved_bytes() <= search_cap {
                break;
            }
            let Some(oldest) = state
                .volumes
                .iter()
                .min_by_key(|(_, e)| e.last_accessed)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            state.volumes.remove(&oldest);
        }
    }

    /// Attempts to obtain an up-to-date `VolumeFileIndex` for an elevated local NTFS volume.
    /// Both cold builds and USN hot refreshes share the same `(drive, VolumeIdentity)` singleflight
    /// so concurrent callers never race or overwrite newer generations.
    #[cfg(windows)]
    pub(crate) fn get_or_build_windows(
        self: &Arc<Self>,
        drive: &str,
        is_subst: bool,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Option<(Arc<VolumeFileIndex>, IndexBuildStatus)>, String> {
        if !mft_enabled() || is_subst || !cst_os::is_elevated().unwrap_or(false) {
            return Ok(None);
        }
        let drive_upper = drive.to_ascii_uppercase();
        let Ok(identity) = crate::diagnostics::volume::volume_identity(&drive_upper) else {
            return Ok(None);
        };
        if !identity.filesystem.eq_ignore_ascii_case("NTFS") {
            return Ok(None);
        }

        // Singleflight registration INSIDE lock (covers BOTH hot USN sync and cold build),
        // thread spawn OUTSIDE lock.
        let key = (drive_upper.clone(), identity.clone());
        let (flight, sub, should_spawn, base_cached) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let ttl = self.idle_ttl;
            state.volumes.retain(|_, e| e.last_accessed.elapsed() < ttl);
            if state
                .usn_unavailable
                .get(&drive_upper)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
            {
                return Ok(None);
            }
            let existing_cached = if let Some(entry) = state.volumes.get_mut(&drive_upper) {
                if entry.index.identity == identity {
                    entry.last_accessed = Instant::now();
                    Some(Arc::clone(&entry.index))
                } else {
                    state.volumes.remove(&drive_upper);
                    None
                }
            } else {
                None
            };

            if let Some(existing) = state
                .pending
                .get(&key)
                .filter(|f| !f.build_cancel.is_cancelled())
            {
                let f = Arc::clone(existing);
                f.subscribers.fetch_add(1, Ordering::AcqRel);
                let sub = FlightSub {
                    manager: Arc::clone(self),
                    key: key.clone(),
                    flight: Arc::clone(&f),
                };
                (f, sub, false, None)
            } else {
                let mut cur = self.active_builds.load(Ordering::Relaxed);
                let acquired = loop {
                    if cur >= MAX_CONCURRENT_MFT_BUILDS {
                        break false;
                    }
                    match self.active_builds.compare_exchange_weak(
                        cur,
                        cur + 1,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break true,
                        Err(next) => cur = next,
                    }
                };
                if !acquired {
                    return Ok(None);
                }
                let f = Arc::new(BuildFlight::new());
                f.subscribers.fetch_add(1, Ordering::AcqRel);
                state.pending.insert(key.clone(), Arc::clone(&f));
                let sub = FlightSub {
                    manager: Arc::clone(self),
                    key: key.clone(),
                    flight: Arc::clone(&f),
                };
                (f, sub, true, existing_cached)
            }
        };

        if should_spawn {
            let mgr = Arc::clone(self);
            let f = Arc::clone(&flight);
            let k = key.clone();
            let d = drive_upper.clone();
            let id = identity.clone();
            let spawn_res = std::thread::Builder::new()
                .name(format!("cst-mft-sync-{d}"))
                .spawn(move || {
                    struct BuildSlotGuard(Arc<SearchIndexManager>);
                    impl Drop for BuildSlotGuard {
                        fn drop(&mut self) {
                            self.0.active_builds.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                    let _slot = BuildSlotGuard(Arc::clone(&mgr));
                    let mut mark_usn_unavailable = false;
                    let outcome =
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            if let Some(ref existing) = base_cached {
                                match win_usn::sync_windows_volume(
                                    existing,
                                    &mgr.ledger,
                                    &f.build_cancel,
                                ) {
                                    Ok(None) => {
                                        return Ok((
                                            Arc::clone(existing),
                                            IndexBuildStatus::UsnSynced,
                                            false,
                                        ));
                                    }
                                    Ok(Some(updated)) => {
                                        return Ok((
                                            Arc::new(updated),
                                            IndexBuildStatus::UsnSynced,
                                            false,
                                        ));
                                    }
                                    Err(
                                        UsnReadError::NotActive
                                        | UsnReadError::AccessDenied
                                        | UsnReadError::UnsupportedVersion,
                                    ) => {
                                        return Err(true);
                                    }
                                    Err(UsnReadError::Wrapped | UsnReadError::IoFailed) => {
                                        if f.build_cancel.is_cancelled() {
                                            return Err(false);
                                        }
                                        // Fall through to cold rebuild within the SAME singleflight.
                                    }
                                }
                            }
                            // Evict LRU before cold build if ledger is near capacity.
                            if let Ok(mut st) = mgr.state.lock() {
                                mgr.evict_lru_locked(&mut st);
                            }
                            match win_usn::cold_build_windows_volume(
                                &d,
                                &id,
                                &mgr.ledger,
                                &f.build_cancel,
                            ) {
                                Ok(idx) => Ok((Arc::new(idx), IndexBuildStatus::ColdBuilt, false)),
                                Err(_) => Err(!f.build_cancel.is_cancelled()),
                            }
                        })) {
                            Ok(Ok((arc, status, _))) => BuildOutcome::Ready(arc, status),
                            Ok(Err(unavail)) => {
                                if f.build_cancel.is_cancelled() {
                                    BuildOutcome::Cancelled
                                } else {
                                    mark_usn_unavailable = unavail;
                                    BuildOutcome::Fallback
                                }
                            }
                            Err(_) => BuildOutcome::Fallback,
                        };

                    // Monotonic publication: verify flight ownership, latest volume identity,
                    // and cursor monotonicity before updating cache; cancellation never deletes cache.
                    if let Ok(mut state) = mgr.state.lock() {
                        let still_active = state
                            .pending
                            .get(&k)
                            .is_some_and(|active| Arc::ptr_eq(active, &f));
                        if still_active {
                            state.pending.remove(&k);
                            state.gen_seq = state.gen_seq.wrapping_add(1);
                            match &outcome {
                                BuildOutcome::Ready(arc, _) => {
                                    let latest_ok = crate::diagnostics::volume::volume_identity(&d)
                                        .is_ok_and(|cur| cur == id);
                                    let monotonic = state.volumes.get(&d).is_none_or(|cur| {
                                        cur.index.identity != id
                                            || cur.index.journal_id != arc.journal_id
                                            || arc.applied_cursor >= cur.index.applied_cursor
                                    });
                                    if latest_ok && monotonic {
                                        state.volumes.insert(
                                            d.clone(),
                                            CachedVolumeEntry {
                                                index: Arc::clone(arc),
                                                last_accessed: Instant::now(),
                                            },
                                        );
                                        state.usn_unavailable.remove(&d);
                                        mgr.evict_lru_locked(&mut state);
                                    }
                                }
                                BuildOutcome::Fallback => {
                                    if mark_usn_unavailable {
                                        state.volumes.remove(&d);
                                        state.usn_unavailable.insert(d.clone(), Instant::now());
                                    }
                                }
                                BuildOutcome::Cancelled => {}
                            }
                        }
                    }
                    f.finish(outcome);
                });

            if spawn_res.is_err() {
                self.active_builds.fetch_sub(1, Ordering::AcqRel);
                if let Ok(mut state) = self.state.lock() {
                    state.pending.remove(&key);
                }
                flight.finish(BuildOutcome::Fallback);
            }
        }

        let outcome = {
            let _keep_sub = sub;
            flight.wait(deadline, cancel)
        };
        match outcome {
            BuildOutcome::Ready(arc, status) => Ok(Some((arc, status))),
            BuildOutcome::Fallback => Ok(None),
            BuildOutcome::Cancelled => Err("文件检索已取消。".into()),
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn get_or_build_windows(
        self: &Arc<Self>,
        _drive: &str,
        _is_subst: bool,
        _deadline: Instant,
        _cancel: &CancellationToken,
    ) -> Result<Option<(Arc<VolumeFileIndex>, IndexBuildStatus)>, String> {
        Ok(None)
    }
}

/// Extract `(drive_upper, optional_subtree_rel)` for a canonical Windows path if it sits on a local drive.
pub(crate) fn split_windows_drive_and_subtree(
    canonical: &Path,
) -> Option<(String, Option<String>)> {
    let norm = normalize_path(canonical);
    let bytes = norm.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let drive = norm[..2].to_ascii_uppercase();
    let rest = norm.get(2..)?.trim_matches('/');
    let subtree = (!rest.is_empty()).then(|| rest.to_owned());
    Some((drive, subtree))
}

#[allow(dead_code)]
pub(crate) fn _keep_path_key_ref(p: &Path) -> String {
    path_key(p)
}

#[cfg(test)]
impl VolumeFileIndex {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_for_test(
        drive_upper: String,
        identity: VolumeIdentity,
        journal_id: u64,
        applied_cursor: i64,
        root_ref: u64,
        objects: Vec<ObjectNode>,
        edges: Vec<NameEdge>,
        name_arena: Vec<u8>,
        ref_to_obj: HashMap<u64, u32>,
        est_bytes_override: Option<usize>,
    ) -> Self {
        let live_objects = objects.len();
        let mut idx = Self {
            drive_upper,
            identity,
            journal_id,
            applied_cursor,
            root_ref,
            objects,
            edges,
            name_arena,
            ref_to_obj,
            live_objects,
            tombstones: 0,
            reachability: ReachabilityState {
                tree_gen: 0,
                dir_depth: Vec::new(),
                dir_parent_edge: Vec::new(),
            },
            estimated_bytes: 0,
            mem_guard: None,
        };
        idx.recompute_reachability();
        idx.estimated_bytes = est_bytes_override.unwrap_or_else(|| {
            Self::compute_estimated_bytes(
                &idx.objects,
                &idx.edges,
                &idx.name_arena,
                &idx.ref_to_obj,
            )
        });
        idx
    }

    pub(crate) fn is_dir_reachable_for_test(&self, obj_idx: usize) -> bool {
        self.reachability
            .dir_depth
            .get(obj_idx)
            .copied()
            .flatten()
            .is_some()
    }
}
