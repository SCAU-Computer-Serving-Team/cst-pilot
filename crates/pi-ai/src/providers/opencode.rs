use std::sync::Arc;

use crate::models::{Provider, ProviderApiConfig, ProviderStreams, create_provider};
use crate::providers::build_standard_provider;
use crate::types::{ProviderHeaders, SimpleStreamOptions, StreamOptions};

const DATA_ZEN: &str = include_str!("data/opencode.json");
const DATA_GO: &str = include_str!("data/opencode-go.json");
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

fn has_header(headers: Option<&ProviderHeaders>, expected: &str) -> bool {
    headers.is_some_and(|map| map.keys().any(|k| k.eq_ignore_ascii_case(expected)))
}

fn inject_session_header_stream(options: Option<&StreamOptions>) -> Option<StreamOptions> {
    let opts = options?;
    let sid = opts.session_id.as_deref().filter(|s| !s.is_empty())?;
    if has_header(opts.headers.as_ref(), OPENCODE_SESSION_HEADER) {
        return None;
    }
    let mut cloned = opts.clone();
    let mut hdrs = cloned.headers.unwrap_or_default();
    hdrs.insert(OPENCODE_SESSION_HEADER.to_owned(), Some(sid.to_owned()));
    cloned.headers = Some(hdrs);
    Some(cloned)
}

fn inject_session_header_simple(
    options: Option<&SimpleStreamOptions>,
) -> Option<SimpleStreamOptions> {
    let opts = options?;
    let patched_base = inject_session_header_stream(Some(&opts.base))?;
    let mut cloned = opts.clone();
    cloned.base = patched_base;
    Some(cloned)
}

fn wrap_opencode_streams(provider: Provider) -> Provider {
    let mut options = provider.to_create_options();
    if let Some(ProviderApiConfig::ByApi(map)) = &mut options.api {
        for streams in map.values_mut() {
            let ProviderStreams {
                stream,
                stream_simple,
            } = streams.clone();
            *streams = ProviderStreams::new(
                Arc::new(move |model, context, opts| {
                    if let Some(patched) = inject_session_header_stream(opts) {
                        stream(model, context, Some(&patched))
                    } else {
                        stream(model, context, opts)
                    }
                }),
                Arc::new(move |model, context, opts| {
                    if let Some(patched) = inject_session_header_simple(opts) {
                        stream_simple(model, context, Some(&patched))
                    } else {
                        stream_simple(model, context, opts)
                    }
                }),
            );
        }
    }
    create_provider(options).expect("built-in OpenCode provider configuration is valid")
}

pub fn opencode_provider() -> Provider {
    wrap_opencode_streams(build_standard_provider(
        "opencode",
        "OpenCode Zen",
        DATA_ZEN,
        false,
        false,
    ))
}

pub fn opencode_go_provider() -> Provider {
    wrap_opencode_streams(build_standard_provider(
        "opencode-go",
        "OpenCode Go",
        DATA_GO,
        false,
        false,
    ))
}
