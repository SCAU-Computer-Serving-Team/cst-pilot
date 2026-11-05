use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pi_session::PromptContextFile;

use crate::auth_storage::strip_utf8_bom;
use crate::skills::resource_path_key;

pub const CONTEXT_FILE_CANDIDATES: &[&str] = &[
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

#[derive(Debug, Clone)]
pub struct GitRepoPaths {
    pub repo_dir: PathBuf,
    pub git_dir: PathBuf,
    pub common_git_dir: PathBuf,
}

/// 定位 `cwd` 所属 Git 仓库的工作树根目录（`repo_dir`）、`.git` 目录（`git_dir`）与主仓库公共 `.git` 目录（`common_git_dir`）。
pub fn find_git_paths(cwd: &Path) -> Option<GitRepoPaths> {
    let mut current = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(cwd)
    };

    loop {
        let dot_git = current.join(".git");
        if let Ok(meta) = fs::metadata(&dot_git) {
            if meta.is_dir() {
                let common = resolve_common_git_dir(&dot_git).unwrap_or_else(|| dot_git.clone());
                return Some(GitRepoPaths {
                    repo_dir: current,
                    git_dir: dot_git,
                    common_git_dir: common,
                });
            } else if meta.is_file()
                && let Ok(text) = fs::read_to_string(&dot_git)
            {
                for line in text.lines() {
                    if let Some(raw_target) = line.trim().strip_prefix("gitdir:") {
                        let target = Path::new(raw_target.trim());
                        let git_dir = if target.is_absolute() {
                            target.to_path_buf()
                        } else {
                            current.join(target)
                        };
                        let common =
                            resolve_common_git_dir(&git_dir).unwrap_or_else(|| git_dir.clone());
                        return Some(GitRepoPaths {
                            repo_dir: current,
                            git_dir,
                            common_git_dir: common,
                        });
                    }
                }
            }
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    None
}

fn resolve_common_git_dir(git_dir: &Path) -> Option<PathBuf> {
    let commondir_file = git_dir.join("commondir");
    let raw = fs::read_to_string(commondir_file).ok()?;
    let rel = raw.trim();
    if rel.is_empty() {
        return None;
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        Some(git_dir.join(p))
    }
}

fn load_context_file_from_dir(dir: &Path) -> Option<PromptContextFile> {
    let entries = fs::read_dir(dir).ok()?;
    let mut exact_names = BTreeSet::new();
    for entry in entries.flatten() {
        if entry.path().is_file() {
            exact_names.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }

    for &candidate in CONTEXT_FILE_CANDIDATES {
        // Check exact case or case-insensitive fallback on Windows if present in dir
        let file_path = dir.join(candidate);
        if (exact_names.contains(candidate) || file_path.is_file())
            && let Ok(raw) = fs::read_to_string(&file_path)
        {
            return Some(PromptContextFile {
                path: file_path.to_string_lossy().into_owned(),
                content: strip_utf8_bom(&raw).to_owned(),
            });
        }
    }
    None
}

fn paths_equal_normalized(a: &Path, b: &Path) -> bool {
    resource_path_key(a) == resource_path_key(b)
}

fn is_path_within_normalized(parent: &Path, child: &Path) -> bool {
    let parent_canon = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    let child_canon = fs::canonicalize(child).unwrap_or_else(|_| child.to_path_buf());
    let p_key = resource_path_key(&parent_canon);
    let c_key = resource_path_key(&child_canon);
    if p_key == c_key {
        return true;
    }
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let prefix = if p_key.ends_with(sep) {
        p_key
    } else {
        format!("{p_key}{sep}")
    };
    c_key.starts_with(&prefix)
}

/// 当 `cwd` 位于嵌套在主仓库目录内部的 linked worktree 且工作树根目录拥有自己的上下文文件时，
/// 返回主仓库根目录下被遮蔽的同名上下文文件路径（对照 `findShadowedContextFile`）。
pub fn find_shadowed_context_file(cwd: &Path) -> Option<PathBuf> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir =
        fs::canonicalize(&git_paths.common_git_dir).unwrap_or(git_paths.common_git_dir);
    let worktree_root = fs::canonicalize(&git_paths.repo_dir).unwrap_or(git_paths.repo_dir);
    let main_repo_root = common_git_dir.parent()?;

    if !is_path_within_normalized(main_repo_root, &worktree_root)
        || paths_equal_normalized(&worktree_root, main_repo_root)
    {
        return None;
    }

    // 确认 main_repo_root/.git 确实就是 common_git_dir（排除 bare 仓库父目录或 submodule）
    let main_dot_git = fs::canonicalize(main_repo_root.join(".git"))
        .unwrap_or_else(|_| main_repo_root.join(".git"));
    if !paths_equal_normalized(&main_dot_git, &common_git_dir) {
        return None;
    }

    let worktree_ctx = load_context_file_from_dir(&worktree_root)?;
    let basename = Path::new(&worktree_ctx.path).file_name()?;
    Some(main_repo_root.join(basename))
}

#[derive(Debug, Clone)]
pub struct LoadContextFilesOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
}

/// 加载 `<agent_dir>` 全局上下文与从根目录到 `cwd` 祖先链上的项目上下文文件（对照 `loadProjectContextFiles`）。
pub fn load_project_context_files(options: &LoadContextFilesOptions) -> Vec<PromptContextFile> {
    let mut context_files = Vec::new();
    let mut seen_keys = BTreeSet::new();

    if let Some(global_ctx) = load_context_file_from_dir(&options.agent_dir) {
        seen_keys.insert(resource_path_key(Path::new(&global_ctx.path)));
        context_files.push(global_ctx);
    }

    let shadowed = find_shadowed_context_file(&options.cwd);
    let mut ancestor_files = Vec::new();
    let mut current_dir = if options.cwd.is_absolute() {
        options.cwd.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&options.cwd)
    };

    loop {
        if let Some(ctx_file) = load_context_file_from_dir(&current_dir) {
            let file_path = Path::new(&ctx_file.path);
            let is_shadowed = if let Some(ref shadow_path) = shadowed
                && let Some(fname) = file_path.file_name()
            {
                let canon_dir =
                    fs::canonicalize(&current_dir).unwrap_or_else(|_| current_dir.clone());
                paths_equal_normalized(&canon_dir.join(fname), shadow_path)
            } else {
                false
            };

            let key = resource_path_key(file_path);
            if !is_shadowed && seen_keys.insert(key) {
                ancestor_files.push(ctx_file);
            }
        }

        let Some(parent) = current_dir.parent() else {
            break;
        };
        if parent == current_dir {
            break;
        }
        current_dir = parent.to_path_buf();
    }

    ancestor_files.reverse();
    context_files.extend(ancestor_files);
    context_files
}
