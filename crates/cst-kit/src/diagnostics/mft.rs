use super::scan::{
    Ranked, STALE_AGE_SECS, STALE_MIN_BYTES, ScanMethod, ScanSnapshot, SensitivePathFilter, TopN,
    ext_key, format_modified_utc, hardlink_rank_key,
};
use super::volume_index::{display_path, drive_key, path_key};
use super::walk::is_cloud_reparse_tag;
use super::win_native::Control;
use ntfs_ng::{
    BootSector, FileReference,
    index::{DirectoryBatch, DirectoryIndex, IndexBuilder, PathPolicy, best_name},
    mft::Mft,
    record::Record,
    records,
    scan::{Chunk, Entry as ScanEntry, Projection, Scan},
    source::{Device, DeviceOptions, ReadAt, read_at},
    time::unix_seconds_u32,
    types,
    value::{FileName, StandardInformation},
};
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_MFT_THREADS: usize = 8;
const MAX_EXTENSION_KEYS: usize = 65_536;
const TOP_LIMIT: usize = 100;
/// Hard record-count cap to bound `DirectoryIndex` + projection peak memory before starting a scan.
const MAX_MFT_RECORDS: u64 = 4_000_000;
/// Hard working-set memory ceiling during MFT projection (falls back to `native-walk` if exceeded).
const MAX_MFT_SCAN_BYTES: usize = 512 * 1024 * 1024;

/// Compact in-memory NTFS volume directory index backed directly by `ntfs-ng::DirectoryIndex`
/// plus a `DirId`-indexed byte-total array and pruned-reparse directory bitmap.
pub struct MftIndex {
    drive_upper: String,
    index: DirectoryIndex,
    dir_bytes: Vec<u64>,
    pruned_dirs: Vec<bool>,
    estimated_bytes: usize,
}

impl MftIndex {
    /// Looks up a directory's aggregated bytes using its live 64-bit NTFS `FileId`
    /// (48-bit MFT record + 16-bit sequence number) AND cross-checks the resolved path
    /// under `PathPolicy::LIVE`. Any sequence mismatch, orphan, pruned reparse directory,
    /// or path mismatch returns `None`.
    pub fn lookup_dir(&self, file_id: Option<u64>, expected_path: &Path) -> Option<u64> {
        let id = file_id.filter(|&v| v != 0)?;
        let expected_rel = self.relative_volume_path(expected_path)?;
        let reference = FileReference::from_u64(id);
        let dir_id = self.index.locate(reference)?;
        if self
            .pruned_dirs
            .get(dir_id as usize)
            .copied()
            .unwrap_or(true)
        {
            return None;
        }
        let indexed_rel = self.index.path(dir_id, '\\');
        if !indexed_rel.eq_ignore_ascii_case(&expected_rel) {
            return None;
        }
        self.dir_bytes.get(dir_id as usize).copied()
    }

    pub fn covers_path(&self, path: &Path) -> bool {
        drive_key(path).is_some_and(|d| d.eq_ignore_ascii_case(&self.drive_upper))
    }

    pub fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }

    fn relative_volume_path(&self, path: &Path) -> Option<String> {
        let disp = display_path(path).replace('/', "\\");
        let trimmed = disp.trim_end_matches('\\');
        let prefix = trimmed.get(..2)?;
        if !prefix.eq_ignore_ascii_case(&self.drive_upper) {
            return None;
        }
        let rest = trimmed.get(2..)?.trim_start_matches('\\');
        Some(rest.to_owned())
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

/// Kill-switch that overrides any enable setting and blocks both MFT scanning and cache reads.
pub fn mft_disabled_by_env() -> bool {
    env_truthy("CST_DISABLE_MFT")
}

/// Raw `$MFT` reads are enabled by default for elevated local NTFS volume roots;
/// set `CST_DISABLE_MFT=1` to force-disable both MFT scanning and cache reads.
pub fn mft_enabled() -> bool {
    !mft_disabled_by_env()
}

#[cfg(windows)]
fn query_volume_alignment(raw_path: &str) -> usize {
    use std::ffi::OsStr;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{
        DISK_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_STORAGE_QUERY_PROPERTY,
        PropertyStandardQuery, STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        StorageAccessAlignmentProperty,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = OsStr::new(raw_path).encode_wide().chain(Some(0)).collect();
    let handle = match unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    } {
        Ok(h) if h != INVALID_HANDLE_VALUE => h,
        _ => return 4096,
    };
    struct Guard(windows::Win32::Foundation::HANDLE);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    let _guard = Guard(handle);

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageAccessAlignmentProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut align_desc: STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR = unsafe { zeroed() };
    let mut ret = 0u32;
    if unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some((&raw mut align_desc).cast()),
            size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>() as u32,
            Some(&mut ret),
            None,
        )
    }
    .is_ok()
        && (ret as usize) >= size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>()
    {
        let physical = align_desc.BytesPerPhysicalSector as usize;
        let logical = align_desc.BytesPerLogicalSector as usize;
        let candidate = physical.max(logical);
        if candidate.is_power_of_two() && (512..=65_536).contains(&candidate) {
            return candidate;
        }
    }

    let mut geom: DISK_GEOMETRY_EX = unsafe { zeroed() };
    ret = 0;
    if unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            None,
            0,
            Some((&raw mut geom).cast()),
            size_of::<DISK_GEOMETRY_EX>() as u32,
            Some(&mut ret),
            None,
        )
    }
    .is_ok()
        && (ret as usize) >= size_of::<DISK_GEOMETRY_EX>()
    {
        let bps = geom.Geometry.BytesPerSector as usize;
        if bps.is_power_of_two() && (512..=65_536).contains(&bps) {
            return bps;
        }
    }
    4096
}

#[cfg(not(windows))]
fn query_volume_alignment(_raw_path: &str) -> usize {
    4096
}

#[derive(Clone, Debug)]
struct ProjectedFile {
    record: u64,
    parent: u64,
    size: u64,
    allocated: u64,
    modified: u32,
    reparse_tag: Option<u32>,
    size_from_name: bool,
    dos: bool,
    alias: bool,
    name_start: u32,
    name_len: u16,
}

#[derive(Clone, Debug)]
struct ListedBase {
    record: u64,
    sequence: u16,
    is_dir: bool,
    size: u64,
    allocated: u64,
    modified: u32,
    reparse_tag: Option<u32>,
    size_from_name: bool,
    first: u32,
    count: u32,
}

#[derive(Clone, Debug)]
struct ExtensionPiece {
    base: FileReference,
    data: Option<(u64, u64)>,
    reparse_tag: Option<u32>,
    names: Vec<(u64, bool, u32, u16)>,
}

#[derive(Default)]
struct ChunkOut {
    first: u64,
    files: Vec<ProjectedFile>,
    names: String,
    listed: Vec<ListedBase>,
    pieces: Vec<ExtensionPiece>,
    directories: DirectoryBatch,
    reparse_dir_records: Vec<u64>,
    attr_parse_errors: u64,
}

impl ChunkOut {
    fn estimated_bytes(&self) -> usize {
        self.files.len() * std::mem::size_of::<ProjectedFile>()
            + self.names.len()
            + self.listed.len() * std::mem::size_of::<ListedBase>()
            + self.pieces.len() * 64
            + self.directories.len() * 64
            + self.reparse_dir_records.len() * 8
    }
}

struct MftProjection;

fn extract_data_sizes(attribute: &ntfs_ng::Attribute<'_>) -> Option<(u64, u64)> {
    if attribute.is_resident() {
        let size = attribute.data_size().ok()?;
        return Some((size, size));
    }
    let extent = attribute.non_resident().ok()?;
    if !extent.is_first() {
        return None;
    }
    let alloc = extent.total_allocated.unwrap_or(extent.allocated_size);
    Some((extent.data_size, alloc))
}

fn extract_reparse_tag(attribute: &ntfs_ng::Attribute<'_>) -> Option<u32> {
    let val = attribute.value().ok()?;
    val.first_chunk::<4>().map(|c| u32::from_le_bytes(*c))
}

impl Projection for MftProjection {
    type Output = ChunkOut;
    type Scratch = Vec<(u64, bool, u32, u16)>;

    fn scratch(&self) -> Self::Scratch {
        Vec::with_capacity(4)
    }

    fn begin(&self, _scratch: &mut Self::Scratch, chunk: Chunk) -> ChunkOut {
        ChunkOut {
            first: chunk.first,
            ..Default::default()
        }
    }

    fn entry(
        &self,
        out: &mut ChunkOut,
        names: &mut Self::Scratch,
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
            Self::extension(out, &record);
            return;
        }
        let is_user = number >= records::FIRST_USER;
        if number == records::ROOT || (header.is_directory() && is_user) {
            out.directories.add(number, header, best_name(&record));
        }
        if !is_user {
            return;
        }
        if header.is_directory() {
            let mut has_reparse = false;
            let mut listed = false;
            for attr in record.attributes() {
                let Ok(attr) = attr else {
                    out.attr_parse_errors += 1;
                    break;
                };
                match attr.type_code() {
                    types::REPARSE_POINT => has_reparse = true,
                    types::ATTRIBUTE_LIST => listed = true,
                    _ => {}
                }
            }
            if has_reparse {
                out.reparse_dir_records.push(number);
            }
            if listed {
                out.listed.push(ListedBase {
                    record: number,
                    sequence: header.sequence,
                    is_dir: true,
                    size: 0,
                    allocated: 0,
                    modified: 0,
                    reparse_tag: has_reparse.then_some(0),
                    size_from_name: false,
                    first: 0,
                    count: 0,
                });
            }
            return;
        }

        // Base live user file record
        let mut modified = 0u32;
        let mut size = 0u64;
        let mut allocated = 0u64;
        let mut data_found = false;
        let mut listed = false;
        let mut reparse_tag: Option<u32> = None;
        names.clear();
        for attr in record.attributes() {
            let Ok(attr) = attr else {
                out.attr_parse_errors += 1;
                break;
            };
            match attr.type_code() {
                types::STANDARD_INFORMATION => {
                    if let Ok(value) = attr.value()
                        && let Some(times) = StandardInformation::times_of(value)
                    {
                        modified = unix_seconds_u32(times.modified);
                    }
                }
                types::FILE_NAME => {
                    if let Ok(value) = attr.value()
                        && let Ok(fn_val) = FileName::parse(value)
                    {
                        let start = out.names.len();
                        fn_val.name.push_lossy(&mut out.names);
                        let len = u16::try_from(out.names.len() - start).unwrap_or(u16::MAX);
                        names.push((
                            fn_val.parent.to_u64(),
                            fn_val.namespace.is_dos_only(),
                            start as u32,
                            len,
                        ));
                    }
                }
                types::DATA if attr.is_unnamed() => {
                    if let Some((s, a)) = extract_data_sizes(&attr) {
                        size = s;
                        allocated = a;
                        data_found = true;
                    }
                }
                types::REPARSE_POINT => {
                    reparse_tag = Some(extract_reparse_tag(&attr).unwrap_or(0));
                }
                types::ATTRIBUTE_LIST => listed = true,
                _ => {}
            }
        }
        let size_from_name = !data_found;
        let first = out.files.len() as u32;
        for &(parent, dos, start, len) in names.iter() {
            if dos
                && names
                    .iter()
                    .any(|&(other, other_dos, ..)| other == parent && !other_dos)
            {
                continue;
            }
            out.files.push(ProjectedFile {
                record: number,
                parent,
                size,
                allocated,
                modified,
                reparse_tag,
                size_from_name,
                dos,
                alias: false,
                name_start: start,
                name_len: len,
            });
        }
        if listed {
            out.listed.push(ListedBase {
                record: number,
                sequence: header.sequence,
                is_dir: false,
                size,
                allocated,
                modified,
                reparse_tag,
                size_from_name,
                first,
                count: out.files.len() as u32 - first,
            });
        }
    }
}

impl MftProjection {
    fn extension(out: &mut ChunkOut, record: &Record<'_>) {
        let header = record.header();
        let mut piece = ExtensionPiece {
            base: header.base_record,
            data: None,
            reparse_tag: None,
            names: Vec::new(),
        };
        for attr in record.attributes() {
            let Ok(attr) = attr else {
                out.attr_parse_errors += 1;
                break;
            };
            match attr.type_code() {
                types::DATA if attr.is_unnamed() => {
                    if let Some(sizes) = extract_data_sizes(&attr) {
                        piece.data = Some(sizes);
                    }
                }
                types::REPARSE_POINT => {
                    piece.reparse_tag = Some(extract_reparse_tag(&attr).unwrap_or(0));
                }
                types::FILE_NAME => {
                    if let Ok(value) = attr.value()
                        && let Ok((parent, namespace, name)) = FileName::link(value)
                    {
                        out.directories.add_extension_name(
                            header.base_record,
                            parent,
                            namespace,
                            name,
                        );
                        let start = out.names.len();
                        name.push_lossy(&mut out.names);
                        let len = u16::try_from(out.names.len() - start).unwrap_or(u16::MAX);
                        piece.names.push((
                            parent.to_u64(),
                            namespace.is_dos_only(),
                            start as u32,
                            len,
                        ));
                    }
                }
                _ => {}
            }
        }
        if piece.data.is_some() || piece.reparse_tag.is_some() || !piece.names.is_empty() {
            out.pieces.push(piece);
        }
    }
}

pub fn scan_ntfs_volume(
    drive: &str,
    volume_guid: Option<&str>,
    control: &Control,
) -> Result<ScanSnapshot, String> {
    scan_ntfs_volume_with_filter(drive, volume_guid, control, &SensitivePathFilter::default())
}

pub fn scan_ntfs_volume_with_filter(
    drive: &str,
    volume_guid: Option<&str>,
    control: &Control,
    filter: &SensitivePathFilter,
) -> Result<ScanSnapshot, String> {
    control.check()?;
    if !mft_enabled() {
        return Err("原生 $MFT 直读已通过 CST_DISABLE_MFT 禁用。".into());
    }
    if drive.len() != 2 || !drive.as_bytes()[0].is_ascii_alphabetic() || drive.as_bytes()[1] != b':'
    {
        return Err("MFT 扫描需要有效本地盘符。".into());
    }
    let drive_upper = drive.to_ascii_uppercase();
    // Prefer stable volume GUID path (`\\?\Volume{...}` without trailing slash) over drive letter
    // so drive-letter reassignment mid-open cannot target another volume.
    let raw_path = volume_guid
        .map(|g| g.trim_end_matches('\\').to_owned())
        .filter(|g| g.starts_with(r"\\?\Volume{"))
        .unwrap_or_else(|| format!(r"\\.\{drive_upper}"));
    let alignment = query_volume_alignment(&raw_path);
    let device = Device::open(
        &raw_path,
        DeviceOptions {
            unbuffered: true,
            alignment,
            len: None,
        },
    )
    .map_err(|e| format!("无法以只读方式打开卷 {drive_upper}（需管理员权限且卷未锁定）：{e}"))?;
    control.check()?;
    let mut boot_buf = [0u8; 512];
    read_at(&device, 0, &mut boot_buf).map_err(|e| format!("读取卷引导扇区失败：{e}"))?;
    let boot = BootSector::parse(&boot_buf).map_err(|e| format!("NTFS 引导扇区无效：{e:?}"))?;
    let mft = Mft::from_volume(&device, &boot).map_err(|e| format!("解析 $MFT 布局失败：{e:?}"))?;
    scan_mft_source_with_filter(&drive_upper, &mft, control, filter)
}

/// Core generic `$MFT` scanning & aggregation pipeline (testable offline with synthetic `Mft`
/// dumps as well as live NTFS volumes). Enforces record/memory limits, `Summary` integrity checks,
/// reparse-directory pruning, deterministic hardlink deduplication, and sensitive path filtering.
pub fn scan_mft_source<S: ReadAt + Sync>(
    drive_upper: &str,
    mft: &Mft<S>,
    control: &Control,
) -> Result<ScanSnapshot, String> {
    scan_mft_source_with_filter(drive_upper, mft, control, &SensitivePathFilter::default())
}

pub fn scan_mft_source_with_filter<S: ReadAt + Sync>(
    drive_upper: &str,
    mft: &Mft<S>,
    control: &Control,
    filter: &SensitivePathFilter,
) -> Result<ScanSnapshot, String> {
    control.check()?;
    let total_records = mft.records();
    if total_records == 0 {
        return Err("$MFT 记录数为 0，无法生成快照。".into());
    }
    if total_records > MAX_MFT_RECORDS {
        return Err(format!(
            "$MFT 记录数（{total_records}）超过单次内存安全上限（{MAX_MFT_RECORDS}），已回退目录遍历。"
        ));
    }
    let estimated_slot_bytes = (total_records as usize).saturating_mul(8);
    if estimated_slot_bytes > MAX_MFT_SCAN_BYTES / 2 {
        return Err("$MFT 预估索引内存超过安全上限，已回退目录遍历。".into());
    }

    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(MAX_MFT_THREADS);

    let cancel_flag = AtomicBool::new(false);
    let done_signal = (std::sync::Mutex::new(false), std::sync::Condvar::new());
    let mut chunks: Vec<ChunkOut> = Vec::new();
    let mut builder = IndexBuilder::new();
    let mut mem_bytes = estimated_slot_bytes;
    let mut mem_exceeded = false;

    let summary = std::thread::scope(|scope| {
        let cf = &cancel_flag;
        let ds = &done_signal;
        let watcher = scope.spawn(move || {
            let (lock, cv) = ds;
            let mut done = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*done {
                if control.check().is_err() {
                    cf.store(true, Ordering::Release);
                    break;
                }
                let (guard, _) = cv
                    .wait_timeout(done, Duration::from_millis(20))
                    .unwrap_or_else(|e| e.into_inner());
                done = guard;
            }
        });
        let scan = Scan::new(mft)
            .threads(threads)
            .chunk_bytes(4 << 20)
            .cancel(&cancel_flag)
            .ordered(false);
        let res = scan.run(&MftProjection, |_chunk, mut out| {
            if control.check().is_err() {
                cf.store(true, Ordering::Release);
                return ControlFlow::Break(());
            }
            mem_bytes = mem_bytes.saturating_add(out.estimated_bytes());
            if mem_bytes > MAX_MFT_SCAN_BYTES {
                mem_exceeded = true;
                cf.store(true, Ordering::Release);
                return ControlFlow::Break(());
            }
            builder.push(std::mem::take(&mut out.directories));
            if !out.files.is_empty()
                || !out.listed.is_empty()
                || !out.pieces.is_empty()
                || !out.reparse_dir_records.is_empty()
                || out.attr_parse_errors > 0
            {
                chunks.push(out);
            }
            ControlFlow::Continue(())
        });
        {
            let (lock, cv) = &done_signal;
            if let Ok(mut done) = lock.lock() {
                *done = true;
                cv.notify_all();
            }
        }
        let _ = watcher.join();
        res
    })
    .map_err(|e| {
        if control.check().is_err() {
            control.check().unwrap_err()
        } else {
            format!("$MFT 扫描失败：{e:?}")
        }
    })?;

    control.check()?;
    if mem_exceeded {
        return Err("$MFT 扫描内存占用超过 512 MiB 上限，已中止并回退目录遍历。".into());
    }
    let attr_errors: u64 = chunks.iter().map(|c| c.attr_parse_errors).sum();
    if summary.unreadable > 0
        || summary.unmapped > 0
        || summary.damaged > 0
        || summary.stopped
        || attr_errors > 0
    {
        return Err(format!(
            "$MFT 扫描存在缺失或损坏记录（不可读 {}、未映射 {}、损坏 {}、属性异常 {}），拒绝发布不完整缓存。",
            summary.unreadable, summary.unmapped, summary.damaged, attr_errors
        ));
    }

    chunks.sort_by_key(|c| c.first);
    let mut reparse_dir_records: HashSet<u64> = HashSet::new();
    for c in &chunks {
        reparse_dir_records.extend(c.reparse_dir_records.iter().copied());
    }
    let extra = fold_extensions(&mut chunks, &mut reparse_dir_records);
    if !extra.files.is_empty() {
        chunks.push(extra);
    }

    let index = builder.build(total_records, PathPolicy::LIVE);
    let Some(root_id) = index.directory(records::ROOT) else {
        return Err("$MFT 缺少根目录记录（record 5），未发布缓存。".into());
    };
    let dir_count = index.len();

    // Mark reparse/cloud directories and propagate pruning top-down so their descendants are never
    // rolled into parent totals.
    let mut pruned_dirs = vec![false; dir_count];
    for &rec_num in &reparse_dir_records {
        if let Some(dir_id) = index.directory(rec_num)
            && dir_id != root_id
            && (dir_id as usize) < dir_count
        {
            pruned_dirs[dir_id as usize] = true;
        }
    }
    let mut by_depth: Vec<(usize, u32)> =
        (0..dir_count as u32).map(|i| (index.depth(i), i)).collect();
    by_depth.sort_unstable_by_key(|a| a.0);
    for &(_, d) in &by_depth {
        if let Some(parent) = index.parent(d)
            && (parent as usize) < dir_count
            && pruned_dirs[parent as usize]
        {
            pruned_dirs[d as usize] = true;
        }
    }

    // Step 1: deterministic hardlink representative selection per MFT record number.
    struct WinnerRef {
        chunk_idx: usize,
        file_idx: usize,
        dir_id: u32,
        bytes: u64,
        modified: u32,
    }
    let mut winners: HashMap<u64, WinnerRef> = HashMap::new();
    let mut untrusted_plain_files = 0u64;
    let mut seen_idx = 0usize;
    for (ci, chunk) in chunks.iter().enumerate() {
        for (fi, file) in chunk.files.iter().enumerate() {
            seen_idx += 1;
            if seen_idx & 0x3FFF == 0 {
                control.check()?;
            }
            if file.alias {
                continue;
            }
            let Some(dir_id) = index.locate(FileReference::from_u64(file.parent)) else {
                continue;
            };
            if (dir_id as usize) >= dir_count
                || !index.in_use(dir_id)
                || pruned_dirs[dir_id as usize]
            {
                continue;
            }
            let bytes = match file.reparse_tag {
                Some(tag) if is_cloud_reparse_tag(tag) => file.size.min(file.allocated),
                Some(_) => continue, // Skip symlinks / AppExecLinks / non-cloud reparse points
                None => {
                    if file.size_from_name {
                        untrusted_plain_files += 1;
                        continue;
                    }
                    file.size
                }
            };
            let name = chunk
                .names
                .get(file.name_start as usize..file.name_start as usize + file.name_len as usize)
                .unwrap_or_default();
            match winners.entry(file.record) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(WinnerRef {
                        chunk_idx: ci,
                        file_idx: fi,
                        dir_id,
                        bytes,
                        modified: file.modified,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut occ) => {
                    let prev = occ.get();
                    let prev_chunk = &chunks[prev.chunk_idx];
                    let prev_file = &prev_chunk.files[prev.file_idx];
                    let prev_name = prev_chunk
                        .names
                        .get(
                            prev_file.name_start as usize
                                ..prev_file.name_start as usize + prev_file.name_len as usize,
                        )
                        .unwrap_or_default();
                    let new_rel = index.path(dir_id, '\\');
                    let old_rel = index.path(prev.dir_id, '\\');
                    if hardlink_rank_key(&new_rel, name) < hardlink_rank_key(&old_rel, prev_name) {
                        *occ.get_mut() = WinnerRef {
                            chunk_idx: ci,
                            file_idx: fi,
                            dir_id,
                            bytes,
                            modified: file.modified,
                        };
                    }
                }
            }
        }
    }
    if untrusted_plain_files > 0 {
        return Err(format!(
            "$MFT 存在 {untrusted_plain_files} 个缺少有效 $DATA 流的文件记录，拒绝发布不完整缓存。"
        ));
    }

    // Step 2: accumulate totals & all 4 rankings from winning entries only.
    let mut dir_bytes = vec![0u64; dir_count];
    let mut extensions: HashMap<String, (u64, u64)> = HashMap::new();
    let mut ext_overflow = false;
    let mut top_files_heap = TopN::new(TOP_LIMIT);
    let mut stale_heap = TopN::new(TOP_LIMIT);
    let now_u32 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    let stale_cutoff = now_u32.saturating_sub(STALE_AGE_SECS as u32);
    let mut file_count = 0usize;

    for (idx, w) in winners.values().enumerate() {
        if idx & 0x3FFF == 0 {
            control.check()?;
        }
        let chunk = &chunks[w.chunk_idx];
        let file = &chunk.files[w.file_idx];
        let name = chunk
            .names
            .get(file.name_start as usize..file.name_start as usize + file.name_len as usize)
            .unwrap_or_default();
        file_count += 1;
        dir_bytes[w.dir_id as usize] = dir_bytes[w.dir_id as usize].saturating_add(w.bytes);
        let ext = ext_key(name);
        if extensions.contains_key(&ext) || extensions.len() < MAX_EXTENSION_KEYS {
            let agg = extensions.entry(ext).or_default();
            agg.0 = agg.0.saturating_add(w.bytes);
            agg.1 = agg.1.saturating_add(1);
        } else {
            ext_overflow = true;
        }
        let rel = if top_files_heap.would_accept(w.bytes)
            || (w.bytes >= STALE_MIN_BYTES
                && w.modified > 0
                && w.modified <= stale_cutoff
                && stale_heap.would_accept(w.bytes))
        {
            Some(index.path(w.dir_id, '\\'))
        } else {
            None
        };
        if let Some(rel) = rel {
            if filter.is_sensitive_mft_entry(drive_upper, &rel, name) {
                continue;
            }
            let full_path = if rel.is_empty() {
                format!("{drive_upper}\\{name}")
            } else {
                format!("{drive_upper}\\{rel}\\{name}")
            };
            if top_files_heap.would_accept(w.bytes) {
                top_files_heap.add(Ranked {
                    bytes: w.bytes,
                    path: full_path.clone(),
                    modified: format_modified_utc(i64::from(w.modified)),
                });
            }
            if w.bytes >= STALE_MIN_BYTES
                && w.modified > 0
                && w.modified <= stale_cutoff
                && stale_heap.would_accept(w.bytes)
            {
                stale_heap.add(Ranked {
                    bytes: w.bytes,
                    path: full_path,
                    modified: format_modified_utc(i64::from(w.modified)),
                });
            }
        }
    }

    // Release file chunks and winner map before building directory rankings.
    drop(winners);
    drop(chunks);

    // Step 3: bottom-up directory roll-up (descending depth).
    for (idx, &(_, i)) in by_depth.iter().rev().enumerate() {
        if idx & 0x3FFF == 0 {
            control.check()?;
        }
        if pruned_dirs[i as usize] {
            continue;
        }
        if let Some(parent) = index.parent(i)
            && parent != i
            && (parent as usize) < dir_count
            && !pruned_dirs[parent as usize]
        {
            dir_bytes[parent as usize] =
                dir_bytes[parent as usize].saturating_add(dir_bytes[i as usize]);
        }
    }

    let total = dir_bytes.get(root_id as usize).copied().unwrap_or(0);
    let mut top_dirs_heap = TopN::new(TOP_LIMIT);
    for dir_id in 0..dir_count as u32 {
        if dir_id == root_id || !index.in_use(dir_id) || pruned_dirs[dir_id as usize] {
            continue;
        }
        let bytes = dir_bytes[dir_id as usize];
        if top_dirs_heap.would_accept(bytes) {
            let rel = index.path(dir_id, '\\');
            if !rel.is_empty() && !filter.is_sensitive_mft_dir(drive_upper, &rel) {
                top_dirs_heap.add(Ranked {
                    bytes,
                    path: format!("{drive_upper}\\{rel}"),
                    modified: None,
                });
            }
        }
    }

    // Conservative estimate of retained resident memory for `VolumeIndex` byte-budget eviction.
    let estimated_bytes = (total_records as usize)
        .saturating_mul(4)
        .saturating_add(dir_count.saturating_mul(96))
        .saturating_add(dir_bytes.len().saturating_mul(8))
        .saturating_add(pruned_dirs.len());
    let mft_index = Arc::new(MftIndex {
        drive_upper: drive_upper.to_owned(),
        index,
        dir_bytes,
        pruned_dirs,
        estimated_bytes,
    });
    let root_str = format!("{drive_upper}\\");
    let _ = path_key(Path::new(&root_str));

    Ok(ScanSnapshot {
        root: root_str,
        identity: None,
        captured_at: Instant::now(),
        method: ScanMethod::NativeMft,
        mft_index: Some(mft_index),
        dirs: HashMap::new(),
        files: HashMap::new(),
        total,
        rows: dir_count + file_count,
        file_count,
        top_dirs: top_dirs_heap.sorted(),
        top_files: top_files_heap.sorted(),
        stale: stale_heap.sorted(),
        extensions,
        ext_overflow,
    })
}

fn fold_extensions(chunks: &mut [ChunkOut], reparse_dir_records: &mut HashSet<u64>) -> ChunkOut {
    let mut listed: Vec<(u64, usize, usize)> = Vec::new();
    for (ci, chunk) in chunks.iter().enumerate() {
        for (li, base) in chunk.listed.iter().enumerate() {
            listed.push((base.record, ci, li));
        }
    }
    if listed.is_empty() {
        return ChunkOut::default();
    }
    listed.sort_unstable_by_key(|&(rec, ..)| rec);
    let mut matched: Vec<(usize, usize, usize)> = Vec::new();
    for (pci, chunk) in chunks.iter().enumerate() {
        for (pi, piece) in chunk.pieces.iter().enumerate() {
            let pos = listed.partition_point(|&(rec, ..)| rec < piece.base.record);
            if let Some(&(rec, bci, bli)) = listed.get(pos)
                && rec == piece.base.record
                && chunks[bci].listed[bli].sequence == piece.base.sequence
            {
                matched.push((pos, pci, pi));
            }
        }
    }
    matched.sort_unstable_by_key(|&(pos, ..)| pos);
    let mut extra = ChunkOut::default();
    let mut idx = 0;
    while idx < matched.len() {
        let pos = matched[idx].0;
        let end = idx + matched[idx..].iter().take_while(|m| m.0 == pos).count();
        let (_, bci, bli) = listed[pos];
        let mut base = chunks[bci].listed[bli].clone();
        for &(_, pci, pi) in &matched[idx..end] {
            let piece = &chunks[pci].pieces[pi];
            if let Some((s, a)) = piece.data {
                base.size = s;
                base.allocated = a;
                base.size_from_name = false;
            }
            if let Some(tag) = piece.reparse_tag {
                base.reparse_tag = Some(tag);
            }
        }
        if base.is_dir {
            if base.reparse_tag.is_some() {
                reparse_dir_records.insert(base.record);
            }
            idx = end;
            continue;
        }
        let extra_first = extra.files.len();
        for &(_, pci, pi) in &matched[idx..end] {
            let piece_names = chunks[pci].pieces[pi].names.clone();
            for (parent, dos, start, len) in piece_names {
                let s = chunks[pci]
                    .names
                    .get(start as usize..start as usize + len as usize)
                    .unwrap_or_default()
                    .to_owned();
                let at = extra.names.len() as u32;
                extra.names.push_str(&s);
                extra.files.push(ProjectedFile {
                    record: base.record,
                    parent,
                    size: base.size,
                    allocated: base.allocated,
                    modified: base.modified,
                    reparse_tag: base.reparse_tag,
                    size_from_name: base.size_from_name,
                    dos,
                    alias: false,
                    name_start: at,
                    name_len: u16::try_from(s.len()).unwrap_or(u16::MAX),
                });
            }
        }
        let own = &mut chunks[bci].files[base.first as usize..(base.first + base.count) as usize];
        for f in own.iter_mut() {
            f.size = base.size;
            f.allocated = base.allocated;
            f.reparse_tag = base.reparse_tag;
            f.size_from_name = base.size_from_name;
        }
        let added = &mut extra.files[extra_first..];
        let long_parents: HashSet<u64> = own
            .iter()
            .chain(added.iter())
            .filter(|f| !f.dos)
            .map(|f| f.parent)
            .collect();
        for f in own.iter_mut().chain(added.iter_mut()) {
            if f.dos && long_parents.contains(&f.parent) {
                f.alias = true;
            }
        }
        idx = end;
    }
    extra
}
