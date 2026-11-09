use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_ai::{ImageContent, ModelThinkingLevel};
use pi_runtime::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeResult, CreateAgentSessionServicesOptions, CreateRuntimeOptions,
    DefaultResourceLoaderOptions, InitialModelPolicy, ModelReferenceRequest, ModelReferenceResult,
    NoToolsMode, PrintModeOptions, PrintOutputMode, ProcessImageOptions, ThinkingSuffixMode,
    create_agent_session_from_services, create_agent_session_runtime,
    create_agent_session_services, detect_supported_image_mime_type, get_default_agent_dir,
    parse_model_thinking_level_cli, process_image_sync, resolve_model_reference, resolve_path,
    run_print_mode, run_rpc_mode,
};
use pi_session::{ExtensionMode, SessionManager};
use tokio::io::BufReader;

#[derive(Debug, Default, Clone)]
struct CliArgs {
    help: bool,
    print: bool,
    mode: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    thinking: Option<ModelThinkingLevel>,
    api_key: Option<String>,
    tools: Option<Vec<String>>,
    no_tools: Option<NoToolsMode>,
    continue_session: bool,
    resume: Option<String>,
    no_session: bool,
    session_dir: Option<PathBuf>,
    agent_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
    system_prompt: Option<String>,
    append_system_prompt: Vec<String>,
    no_extensions: bool,
    no_skills: bool,
    no_prompt_templates: bool,
    no_context_files: bool,
    list_models: bool,
    positionals: Vec<String>,
}

fn parse_cli_args(raw_args: &[String]) -> Result<CliArgs, String> {
    let mut args = CliArgs::default();
    let mut i = 0usize;

    while i < raw_args.len() {
        let arg = &raw_args[i];
        match arg.as_str() {
            "-h" | "--help" => args.help = true,
            "-p" | "--print" => args.print = true,
            "--mode" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--mode requires a value (text, json, or rpc)".to_owned())?;
                if !matches!(val.as_str(), "text" | "json" | "rpc") {
                    return Err(format!(
                        "Invalid --mode \"{val}\": expected text, json, or rpc"
                    ));
                }
                args.mode = Some(val.clone());
            }
            "--model" | "-m" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--model requires a value".to_owned())?;
                args.model = Some(val.clone());
            }
            "--provider" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--provider requires a value".to_owned())?;
                args.provider = Some(val.clone());
            }
            "--thinking" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--thinking requires a value".to_owned())?;
                let lvl = parse_model_thinking_level_cli(val)
                    .ok_or_else(|| format!("Invalid thinking level \"{val}\""))?;
                args.thinking = Some(lvl);
            }
            "--api-key" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--api-key requires a value".to_owned())?;
                args.api_key = Some(val.clone());
            }
            "--tools" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--tools requires a value".to_owned())?;
                let items: Vec<String> = val
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                args.tools.get_or_insert_with(Vec::new).extend(items);
            }
            "--no-tools" => {
                if let Some(next) = raw_args.get(i + 1)
                    && (next == "all" || next == "builtin")
                {
                    i += 1;
                    args.no_tools = Some(if next == "builtin" {
                        NoToolsMode::Builtin
                    } else {
                        NoToolsMode::All
                    });
                } else {
                    args.no_tools = Some(NoToolsMode::All);
                }
            }
            "-c" | "--continue" => args.continue_session = true,
            "-r" | "--resume" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--resume requires a session ID or file path".to_owned())?;
                args.resume = Some(val.clone());
            }
            "--no-session" => args.no_session = true,
            "--session-dir" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--session-dir requires a directory path".to_owned())?;
                args.session_dir = Some(PathBuf::from(val));
            }
            "--agent-dir" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--agent-dir requires a directory path".to_owned())?;
                args.agent_dir = Some(PathBuf::from(val));
            }
            "--cwd" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--cwd requires a directory path".to_owned())?;
                args.cwd = Some(PathBuf::from(val));
            }
            "--system-prompt" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--system-prompt requires a value".to_owned())?;
                args.system_prompt = Some(val.clone());
            }
            "--append-system-prompt" => {
                i += 1;
                let val = raw_args
                    .get(i)
                    .ok_or_else(|| "--append-system-prompt requires a value".to_owned())?;
                args.append_system_prompt.push(val.clone());
            }
            "-ne" | "--no-extensions" => args.no_extensions = true,
            "--no-skills" => args.no_skills = true,
            "--no-prompt-templates" => args.no_prompt_templates = true,
            "--no-context-files" => args.no_context_files = true,
            "--list-models" => args.list_models = true,
            other if other.starts_with('-') => {
                return Err(format!("Unknown option: {other}"));
            }
            positional => {
                args.positionals.push(positional.to_owned());
            }
        }
        i += 1;
    }

    Ok(args)
}

fn print_help() {
    println!(
        "pi - Rust Coding Agent Runtime CLI

Usage: pi [options] [prompt...] [@file...]

Options:
  -p, --print                         Run in single-shot print mode
  --mode <text|json|rpc>              Output/interaction mode (default: text)
  -m, --model <pattern>               Model reference (provider/id or id[:thinking])
  --provider <id>                     Provider constraint for --model
  --thinking <level>                  Thinking level (off|minimal|low|medium|high|xhigh|max)
  --api-key <key>                     Runtime API key override for the selected provider
  --tools <list>                      Comma-separated tool allowlist or +name/-name modifiers
  --no-tools [all|builtin]            Disable built-in or all tools
  -c, --continue                      Continue most recent session in cwd
  -r, --resume <id_or_path>           Resume session by ID prefix or JSONL file path
  --no-session                        Use ephemeral in-memory session
  --session-dir <dir>                 Custom directory for session JSONL files
  --agent-dir <dir>                   Custom global agent configuration directory
  --cwd <dir>                         Working directory override
  --system-prompt <text_or_file>      Override system prompt
  --append-system-prompt <text_or_file> Append to system prompt
  -ne, --no-extensions                Disable extensions
  --no-skills                         Disable skills loading
  --no-prompt-templates               Disable prompt templates loading
  --no-context-files                  Disable AGENTS.md / CLAUDE.md loading
  --list-models                       List available models and exit
  -h, --help                          Show this help message"
    );
}

type InitialMessage = (Option<String>, Option<Vec<ImageContent>>, Vec<String>);

fn build_initial_message_and_images(
    positionals: &[String],
    piped_stdin: Option<String>,
    cwd: &Path,
) -> Result<InitialMessage, String> {
    let mut file_blocks = Vec::new();
    let mut images = Vec::new();
    let mut plain_prompts = Vec::new();

    for pos in positionals {
        if let Some(at_path) = pos.strip_prefix('@')
            && !at_path.is_empty()
        {
            let resolved = resolve_path(at_path, Some(cwd));
            if !resolved.is_file() {
                return Err(format!(
                    "File not found for @{at_path}: {}",
                    resolved.display()
                ));
            }
            if let Some(mime) = detect_supported_image_mime_type(&resolved)? {
                let raw = fs::read(&resolved)
                    .map_err(|e| format!("Failed to read image '{}': {e}", resolved.display()))?;
                let processed = process_image_sync(
                    &raw,
                    &mime,
                    &ProcessImageOptions {
                        auto_resize_images: Some(true),
                        resize_options: None,
                    },
                );
                if processed.ok {
                    if !processed.hints.is_empty() {
                        file_blocks.push(processed.hints.join("\n"));
                    }
                    images.push(ImageContent {
                        data: processed.data,
                        mime_type: processed.mime_type,
                    });
                } else if let Some(msg) = processed.message {
                    file_blocks.push(msg);
                }
            } else {
                let content = fs::read_to_string(&resolved)
                    .map_err(|e| format!("Failed to read '{}': {e}", resolved.display()))?;
                file_blocks.push(format!("<file path=\"{at_path}\">\n{content}\n</file>"));
            }
        } else {
            plain_prompts.push(pos.clone());
        }
    }

    let mut initial_parts = Vec::new();
    if let Some(stdin_text) = piped_stdin
        && !stdin_text.trim().is_empty()
    {
        initial_parts.push(stdin_text);
    }
    if !file_blocks.is_empty() {
        initial_parts.push(file_blocks.join("\n\n"));
    }

    let mut remaining_messages = Vec::new();
    if !plain_prompts.is_empty() {
        initial_parts.push(plain_prompts[0].clone());
        remaining_messages.extend(plain_prompts.into_iter().skip(1));
    }

    let initial_message = if initial_parts.is_empty() {
        if !images.is_empty() {
            Some("Read the attached image(s).".to_owned())
        } else {
            None
        }
    } else {
        Some(initial_parts.join("\n\n"))
    };

    let initial_images = (!images.is_empty()).then_some(images);
    Ok((initial_message, initial_images, remaining_messages))
}

#[tokio::main]
async fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let code = run_cli(raw_args).await;
    std::process::exit(code);
}

async fn run_cli(raw_args: Vec<String>) -> i32 {
    let args = match parse_cli_args(&raw_args) {
        Ok(a) => a,
        Err(err) => {
            eprintln!("Error: {err}");
            return 1;
        }
    };

    if args.help {
        print_help();
        return 0;
    }

    let cwd = args
        .cwd
        .clone()
        .map(|p| resolve_path(p, None))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let agent_dir = args
        .agent_dir
        .clone()
        .map(|p| resolve_path(p, Some(&cwd)))
        .unwrap_or_else(get_default_agent_dir);

    let rl_opts = DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        no_extensions: args.no_extensions,
        no_skills: args.no_skills,
        no_prompt_templates: args.no_prompt_templates,
        no_context_files: args.no_context_files,
        system_prompt: args.system_prompt.clone(),
        append_system_prompt: args.append_system_prompt.clone(),
        ..DefaultResourceLoaderOptions::default()
    };

    if args.list_models {
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd,
            agent_dir: Some(agent_dir),
            resource_loader_options: Some(rl_opts),
            ..CreateAgentSessionServicesOptions::default()
        })
        .await;
        let models = services.model_runtime.get_all_models();
        for m in models {
            let auth_tag = if services.model_runtime.has_configured_auth(&m.provider) {
                " [ready]"
            } else {
                ""
            };
            println!("{}/{}{}", m.provider, m.id, auth_tag);
        }
        return 0;
    }

    let mode_str = args.mode.as_deref().unwrap_or("text");
    let is_rpc = mode_str == "rpc";

    let piped_stdin = if !is_rpc && !std::io::stdin().is_terminal() {
        use std::io::Read;
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        let trimmed = buf.trim().to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    } else {
        None
    };

    let (initial_message, initial_images, messages) =
        match build_initial_message_and_images(&args.positionals, piped_stdin, &cwd) {
            Ok(tuple) => tuple,
            Err(err) => {
                eprintln!("Error: {err}");
                return 1;
            }
        };

    let cwd_str = cwd.to_string_lossy().into_owned();
    let session_dir = args
        .session_dir
        .clone()
        .map(|d| resolve_path(d, Some(&cwd)))
        .unwrap_or_else(|| pi_runtime::get_default_session_dir(&cwd, &agent_dir));

    let session_manager_res = if args.no_session {
        SessionManager::in_memory(&cwd_str, None, None)
    } else if let Some(ref resume_arg) = args.resume {
        let candidate_path = resolve_path(resume_arg, Some(&cwd));
        if candidate_path.is_file() {
            SessionManager::open(candidate_path, Some(session_dir.clone()), None)
        } else if let Some(found_path) =
            pi_session::find_session_by_id(&session_dir, resume_arg, Some(&cwd_str))
        {
            SessionManager::open(found_path, Some(session_dir.clone()), None)
        } else {
            eprintln!("Error: Session \"{resume_arg}\" not found.");
            return 1;
        }
    } else if args.continue_session {
        SessionManager::continue_recent(&cwd_str, session_dir.clone())
    } else {
        SessionManager::create(&cwd_str, &session_dir, None)
    };

    let session_manager = match session_manager_res {
        Ok(sm) => sm,
        Err(err) => {
            eprintln!("Error: {err}");
            return 1;
        }
    };

    let args_for_factory = args.clone();
    let rl_opts_for_factory = rl_opts.clone();
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(
        move |rt_opts: CreateRuntimeOptions| {
            let a = args_for_factory.clone();
            let mut rlo = rl_opts_for_factory.clone();
            rlo.cwd = rt_opts.cwd.clone();
            rlo.agent_dir = rt_opts.agent_dir.clone();

            Box::pin(async move {
                let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                    cwd: rt_opts.cwd.clone(),
                    agent_dir: Some(rt_opts.agent_dir.clone()),
                    resource_loader_options: Some(rlo),
                    ..CreateAgentSessionServicesOptions::default()
                })
                .await;

                let mut explicit_model = None;
                let mut explicit_thinking = a.thinking;

                if let Some(ref cli_model) = a.model {
                    let all_models = services.model_runtime.get_all_models();
                    let inline_key = a.api_key.is_some();
                    let res = resolve_model_reference(
                        &ModelReferenceRequest {
                            text: cli_model.clone(),
                            provider: a.provider.clone(),
                            thinking_suffix: ThinkingSuffixMode::Strict,
                            fuzzy: true,
                        },
                        &all_models,
                        |prov| {
                            if inline_key {
                                false
                            } else {
                                services.model_runtime.has_configured_auth(prov)
                            }
                        },
                    );
                    match res {
                        ModelReferenceResult::Resolved {
                            model,
                            thinking_level,
                            ..
                        } => {
                            explicit_model = Some(*model);
                            if explicit_thinking.is_none() {
                                explicit_thinking = thinking_level;
                            }
                        }
                        ModelReferenceResult::Ambiguous { candidates, .. } => {
                            let list: Vec<String> = candidates
                                .iter()
                                .map(|m| format!("{}/{}", m.provider, m.id))
                                .collect();
                            return Err(format!(
                                "Model \"{cli_model}\" is ambiguous across providers: {}. Use --provider or provider/model.",
                                list.join(", ")
                            ));
                        }
                        ModelReferenceResult::NotFound { target } => {
                            if let Some(t) = target {
                                let mut base = t.provider_models[0].clone();
                                base.id = t.model_id;
                                base.name = base.id.clone();
                                if explicit_thinking.is_none() {
                                    explicit_thinking = t.thinking_level;
                                }
                                explicit_model = Some(base);
                            } else {
                                return Err(format!(
                                    "Model \"{cli_model}\" not found. Use --list-models to see available models."
                                ));
                            }
                        }
                    }
                }

                if let Some(ref key) = a.api_key {
                    let target_prov = explicit_model
                        .as_ref()
                        .map(|m| m.provider.clone())
                        .or_else(|| a.provider.clone());
                    if let Some(prov) = target_prov {
                        let _ = services.model_runtime.set_runtime_api_key(&prov, key).await;
                    }
                }

                let ext_mode = match a.mode.as_deref() {
                    Some("rpc") => ExtensionMode::Rpc,
                    _ => ExtensionMode::Print,
                };

                let created =
                    create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                        services: services.clone(),
                        session_manager: rt_opts.session_manager,
                        execution_log: None,
                        execution_log_factory: None,
                        data_policy: None,
                        session_start_event: rt_opts.session_start_event,
                        model: explicit_model,
                        thinking_level: explicit_thinking,
                        scoped_models: Vec::new(),
                        initial_model_policy: InitialModelPolicy::GenericSdk,
                        no_tools: a.no_tools,
                        tools: a.tools.clone(),
                        exclude_tools: None,
                        custom_tools: Vec::new(),
                        tools_options: None,
                        ui_context: None,
                        mode: Some(ext_mode),
                        tool_execution: None,
                        clock: None,
                    })
                    .await?;

                Ok(CreateAgentSessionRuntimeResult {
                    session: created.session,
                    services,
                    extensions_result: created.extensions_result,
                    diagnostics: Vec::new(),
                    model_fallback_message: created.model_fallback_message,
                })
            })
        },
    );

    let runtime_host = match create_agent_session_runtime(
        factory,
        CreateRuntimeOptions {
            cwd,
            agent_dir,
            session_manager,
            session_start_event: Some(("startup".to_owned(), None)),
        },
    )
    .await
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("Error: {err}");
            return 1;
        }
    };

    if is_rpc {
        let stdin = BufReader::new(tokio::io::stdin());
        let stdout = tokio::io::stdout();
        let stderr = tokio::io::stderr();
        run_rpc_mode(&runtime_host, stdin, stdout, stderr).await
    } else {
        let out_mode = if mode_str == "json" {
            PrintOutputMode::Json
        } else {
            PrintOutputMode::Text
        };
        let stdout = tokio::io::stdout();
        let stderr = tokio::io::stderr();
        run_print_mode(
            &runtime_host,
            PrintModeOptions {
                mode: out_mode,
                initial_message,
                initial_images,
                messages,
            },
            stdout,
            stderr,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    #[test]
    fn file_expansion_preserves_stdin_order_image_bytes_and_remaining_prompts() {
        let root = std::env::temp_dir().join(format!("pi-cli-input-{}", pi_ai::uuid_v7()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("notes.txt"), "file body\n").unwrap();
        fs::write(root.join("image.png"), [1, 2, 3]).unwrap();
        let (initial, images, remaining) = build_initial_message_and_images(
            &[
                "@notes.txt".into(),
                "@image.png".into(),
                "first".into(),
                "second".into(),
            ],
            Some("piped text".into()),
            &root,
        )
        .unwrap();
        assert_eq!(
            initial.as_deref(),
            Some("piped text\n\n<file path=\"notes.txt\">\nfile body\n\n</file>\n\nfirst")
        );
        assert_eq!(remaining, ["second"]);
        let images = images.unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(BASE64_STANDARD.decode(&images[0].data).unwrap(), [1, 2, 3]);
        assert_eq!(images[0].mime_type, "image/png");
        let (initial, _, _) =
            build_initial_message_and_images(&["@image.png".into()], None, &root).unwrap();
        assert_eq!(initial.as_deref(), Some("Read the attached image(s)."));
        assert_eq!(
            build_initial_message_and_images(&[], Some(" \n".into()), &root).unwrap(),
            (None, None, vec![])
        );
        fs::remove_dir_all(root).unwrap();
    }
}
