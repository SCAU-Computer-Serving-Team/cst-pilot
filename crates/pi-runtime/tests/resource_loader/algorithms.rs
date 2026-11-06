use pi_runtime::*;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    let p = std::env::temp_dir()
        .join("cst-pilot")
        .join(format!("resource-algorithms-{}", pi_ai::uuid_v7()));
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn put(path: impl AsRef<Path>, text: &str) {
    let p = path.as_ref();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}
fn skill(dir: &Path, name: &str) {
    put(
        dir.join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: skill description\n---\nbody"),
    );
}

#[test]
fn skills_ignore_matches_git_rules_and_skill_roots_stop_recursion() {
    let root = root();
    put(
        root.join(".gitignore"),
        "[ab]-ignored/\nkeep.md/\n**/skip-*\n",
    );
    put(root.join(".ignore"), "!b-ignored/\n");
    skill(&root.join("a-ignored"), "ignored");
    skill(&root.join("b-ignored"), "restored");
    skill(&root.join("ordinary/skip-me"), "skip");
    skill(&root.join("ordinary/valid"), "valid");
    put(
        root.join("keep.md"),
        "---\nname: root-file\ndescription: keep file despite directory-only rule\n---\nbody",
    );
    skill(&root.join("container"), "container");
    skill(&root.join("container/nested"), "hidden-nested");
    let loaded = load_skills_from_dir(&root, "project");
    let mut names = loaded
        .skills
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["container", "restored", "root-file", "valid"]);
    // A declared root ends discovery even if the root skill has an empty description.
    put(
        root.join("SKILL.md"),
        "---\nname: root\ndescription: ''\n---\nbody",
    );
    let loaded = load_skills_from_dir(&root, "project");
    assert!(loaded.skills.is_empty());
    assert!(!loaded.diagnostics.is_empty());
}

#[test]
fn skill_validation_counts_characters_and_deduplicates_paths_and_names() {
    let root = root();
    let agent = root.join("agent");
    let cwd = root.join("project");
    skill(&agent.join("skills/first"), "same");
    skill(&cwd.join(".pi/skills/second"), "same");
    put(
        agent.join("skills/chinese/SKILL.md"),
        &format!(
            "---\nname: bad--name\ndescription: {}\ndisable-model-invocation: true\n---\nbody",
            "中".repeat(1024)
        ),
    );
    let result = load_skills(&LoadSkillsOptions {
        cwd,
        agent_dir: agent.clone(),
        skill_paths: vec![agent.join("skills/first/SKILL.md")],
        include_defaults: true,
    });
    assert_eq!(result.skills.len(), 2);
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|d| d.kind == "collision")
            .count(),
        1
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("description exceeds"))
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.contains("consecutive hyphens"))
    );
    let xml = format_skills_for_prompt(&result.skills, SkillReadToolKind::Bash);
    assert!(xml.contains("Use bash"));
    assert!(!xml.contains("bad--name"));
}

#[test]
fn template_arguments_are_single_pass_and_support_defaults_and_slices() {
    assert_eq!(
        parse_command_args("one \"\" '' 'two three' a\"b\""),
        ["one", "", "", "two three", "ab"]
    );
    let args = vec!["$2".into(), "second".into(), "".into(), "fourth".into()];
    assert_eq!(
        substitute_args(
            "$1|$2|${3:-empty}|${5:-missing}|${@:0:2}|${@:2:2}|${ARGUMENTS:-fallback}",
            &args
        ),
        "$2|second|empty|missing|$2 second|second |$2 second  fourth"
    );
    assert_eq!(
        substitute_args("${@:-none}|${ARGUMENTS:-none}", &[]),
        "none|none"
    );
    assert_eq!(
        substitute_args("${@:2:18446744073709551615}", &args),
        "second  fourth"
    );
}

#[test]
fn templates_load_direct_markdown_only_keep_first_and_report_collisions() {
    let root = root();
    let agent = root.join("agent");
    let cwd = root.join("project");
    put(
        agent.join("prompts/review.md"),
        "\u{feff}---\ndescription: global\nargument-hint: '[file]'\n---\nglobal $1",
    );
    put(agent.join("prompts/nested/ignored.md"), "not scanned");
    put(cwd.join(".pi/prompts/review.md"), "project");
    let loaded = load_prompt_templates(&LoadPromptTemplatesOptions {
        cwd,
        agent_dir: agent.clone(),
        prompt_paths: vec![agent.join("prompts/review.md")],
        include_defaults: true,
    });
    assert_eq!(loaded.templates.len(), 1);
    assert_eq!(loaded.diagnostics.len(), 1);
    assert_eq!(loaded.templates[0].argument_hint.as_deref(), Some("[file]"));
    assert_eq!(
        expand_prompt_template("/review 'a b'", &loaded.templates),
        "global a b"
    );
}

#[test]
fn contexts_use_precedence_bom_and_nested_worktree_shadowing() {
    let root = root();
    let agent = root.join("agent");
    let main = root.join("main");
    let work = main.join("worktrees/test");
    let cwd = work.join("subdir");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(main.join(".git/worktrees/test")).unwrap();
    put(agent.join("AGENTS.md"), "\u{feff}global");
    put(main.join("AGENTS.md"), "main");
    put(work.join(".git"), "gitdir: ../../.git/worktrees/test\n");
    put(main.join(".git/worktrees/test/commondir"), "../..\n");
    put(work.join("AGENTS.md"), "worktree");
    put(cwd.join("AGENTS.override.md"), "\u{feff}override");
    put(cwd.join("AGENTS.md"), "lower priority");
    let loaded = load_project_context_files(&LoadContextFilesOptions {
        cwd,
        agent_dir: agent,
    });
    let in_fixture = loaded
        .iter()
        .filter(|f| Path::new(&f.path).starts_with(&root))
        .map(|f| f.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(in_fixture, ["global", "worktree", "override"]);
}
