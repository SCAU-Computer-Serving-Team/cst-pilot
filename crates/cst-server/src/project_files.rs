use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FILE_CACHE_TTL: Duration = Duration::from_secs(30);
const FILE_SCAN_LIMIT: usize = 4_000;
const FILE_SCAN_MAX_DEPTH: usize = 6;

struct CachedFiles {
    at: Instant,
    root: String,
    files: Vec<String>,
}

#[derive(Clone, Default)]
pub struct ProjectFileCache {
    inner: Arc<Mutex<Option<CachedFiles>>>,
}

impl ProjectFileCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self, root: &Path) -> Vec<String> {
        let root_str = root.to_string_lossy().into_owned();
        {
            let guard = self.inner.lock().expect("file cache lock");
            if let Some(cached) = &*guard
                && cached.root == root_str
                && cached.at.elapsed() < FILE_CACHE_TTL
            {
                return cached.files.clone();
            }
        }

        let skip: HashSet<&'static str> = [
            "node_modules",
            ".git",
            "dist",
            "build",
            "out",
            "coverage",
            "vendor",
            "tmp",
        ]
        .into_iter()
        .collect();

        let mut files = Vec::new();
        Self::walk(root, "", 0, &skip, &mut files);
        files.sort();

        *self.inner.lock().expect("file cache lock") = Some(CachedFiles {
            at: Instant::now(),
            root: root_str,
            files: files.clone(),
        });
        files
    }

    fn walk(
        dir: &Path,
        prefix: &str,
        depth: usize,
        skip: &HashSet<&'static str>,
        files: &mut Vec<String>,
    ) {
        if files.len() >= FILE_SCAN_LIMIT || depth > FILE_SCAN_MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            if files.len() >= FILE_SCAN_LIMIT {
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if !skip.contains(name.as_str()) {
                    Self::walk(
                        &entry.path(),
                        &format!("{prefix}{name}/"),
                        depth + 1,
                        skip,
                        files,
                    );
                }
            } else if ft.is_file() {
                files.push(format!("{prefix}{name}"));
            }
        }
    }
}
