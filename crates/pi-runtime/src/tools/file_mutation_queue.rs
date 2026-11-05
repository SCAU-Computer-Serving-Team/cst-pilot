use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::sync::oneshot;

/// 解析文件变更队列的规范化路径键：
/// 1. 将输入路径转为绝对路径并折叠 `.` / `..`。
/// 2. 向上查找最长已存在的祖先目录并执行 `fs::canonicalize`（解析父目录符号链接），再拼接尚未创建的子路径段。
/// 3. 在 Windows 下统一剥离 `\\?\` 前缀、将 `/` 转为 `\` 并转为小写，确保大小写别名与分隔符别名映射到同一个队列锁。
pub fn resolve_mutation_queue_key(file_path: &Path) -> String {
    let abs = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(file_path)
    };
    // Resolve existing prefixes before folding "..": a symlink's parent can differ
    // from its lexical parent. Keep all uncreated components until after canonicalization.
    let components: Vec<_> = abs.components().collect();
    let mut canonical_combined = None;
    for n in (1..=components.len()).rev() {
        let prefix: PathBuf = components[..n].iter().collect();
        if let Ok(mut resolved) = fs::canonicalize(&prefix) {
            for component in &components[n..] {
                resolved.push(component.as_os_str());
            }
            canonical_combined = Some(lexical_clean(&resolved));
            break;
        }
    }
    let canonical_combined = canonical_combined.unwrap_or_else(|| lexical_clean(&abs));

    let raw = canonical_combined.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        raw.strip_prefix(r"\\?\")
            .unwrap_or(&raw)
            .replace('/', "\\")
            .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        raw
    }
}

fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_)) | None
                ) {
                    out.pop();
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    out
}

type FileLockMap = Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>;

fn global_file_locks() -> &'static FileLockMap {
    static LOCKS: OnceLock<FileLockMap> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn get_lock_for_key(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = global_file_locks().lock().expect("file locks map");
    map.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = map.get(key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    map.insert(key.to_owned(), Arc::downgrade(&lock));
    lock
}

/// 串行化针对同一文件的变更操作（不同文件并行执行）。
///
/// **Drop 安全保证**：实际的文件操作 `op` 运行在独立的受控后台任务中，并由该任务持有该文件的异步互斥锁守卫，
/// 直至 `op` 彻底执行完毕才释放锁。即使调用方等待的 Future 在中途被 `abort` 或 `drop`，
/// 下一个排队的同文件写操作也必定等待当前底层磁盘写入结算后才能进入。
pub async fn with_file_mutation_queue<T, F, Fut>(
    file_path: impl AsRef<Path>,
    op: F,
) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>> + Send + 'static,
{
    let path_buf = file_path.as_ref().to_path_buf();
    let (tx, rx) = oneshot::channel::<Result<T, String>>();

    tokio::spawn(async move {
        let key = tokio::task::spawn_blocking(move || resolve_mutation_queue_key(&path_buf)).await;
        let key = match key {
            Ok(key) => key,
            Err(_) => {
                let _ = tx.send(Err("Failed to resolve mutation path".to_owned()));
                return;
            }
        };
        let file_mutex = get_lock_for_key(&key);
        let _guard = file_mutex.lock().await;
        let res = op().await;
        let _ = tx.send(res);
        // `_guard` 在 `op().await` 彻底完成后才在此处释放
    });

    rx.await
        .unwrap_or_else(|_| Err("File mutation task was unexpectedly cancelled".to_owned()))
}
