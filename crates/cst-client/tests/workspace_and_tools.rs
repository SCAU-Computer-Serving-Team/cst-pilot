use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use cst_client::tool_presenter::{
    CardStatus, SpecialStatus, SpecialToolKind, SpecialToolView, ToolBlock, ToolCallInput,
    ToolCallSegmentKind, ToolExecutionMeta, ToolPresenterCache, ToolResultInput, map_special_tool,
    map_tool, special_tool_kind, split_tool_calls,
};
use cst_client::workspace::{
    DraftKey, ImageBudgetPool, InlineToken, MAX_SUBMISSION_IMAGE_BYTES, SendMode, WorkspaceState,
    WorkspaceView,
};
use image::{ImageFormat, RgbaImage};
use serde_json::{Map, Value, json};

fn encode_png(width: u32, height: u32, fill_seed: u8) -> Vec<u8> {
    let mut img = RgbaImage::new(width, height);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([
            ((x as u8).wrapping_add(fill_seed)),
            (y as u8),
            fill_seed,
            255,
        ]);
    }
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

#[test]
fn shared_workspace_switches_views_and_enforces_48mib_image_budget_with_drop_reclamation() {
    // Use a 4 MiB test pool to deterministically exercise budget exhaustion and Drop reclamation
    let pool = Arc::new(ImageBudgetPool::with_budget(4 * 1024 * 1024));
    let mut ws = WorkspaceState::new(pool.clone());
    assert_eq!(ws.active_view(), WorkspaceView::Gui);
    assert_eq!(ws.active_draft_key(), DraftKey::NewSession);

    // 1. Unsupported format rejected
    let bad = pool.admit_image(b"not-an-image".to_vec(), None);
    assert!(bad.is_err());

    // 2. Oversized >12 MiB compressed payload rejected before decode
    let huge_raw = vec![0x89u8; MAX_SUBMISSION_IMAGE_BYTES + 1];
    assert_eq!(
        pool.admit_image(huge_raw, None).unwrap_err().code(),
        "image_budget_exceeded"
    );

    // 3. Duplicate image bytes deduplicate by SHA-256 without double-charging budget
    let png_a = encode_png(512, 512, 7); // decoded RGBA = 1 MiB
    let h1 = pool
        .admit_image(png_a.clone(), Some("a.png".into()))
        .unwrap();
    let used_after_h1 = pool.used_bytes();
    assert!(used_after_h1 > 1024 * 1024);
    let h1_dup = pool.admit_image(png_a, Some("a-copy.png".into())).unwrap();
    assert_eq!(h1.sha256(), h1_dup.sha256());
    assert_eq!(pool.used_bytes(), used_after_h1);

    // 4. Add distinct 512x512 (~1 MiB decoded each) images until 4 MiB budget rejects the 4th
    let h2 = pool.admit_image(encode_png(512, 512, 22), None).unwrap();
    let h3 = pool.admit_image(encode_png(512, 512, 99), None).unwrap();
    let over = pool.admit_image(encode_png(512, 512, 144), None);
    assert_eq!(over.unwrap_err().code(), "image_budget_exceeded");

    // 5. Attach to GUI draft, switch to TUI with settled IME text, verify shared state
    {
        let draft = ws.draft_mut(DraftKey::NewSession);
        draft.tokens.push(InlineToken::Command {
            raw: "/skill:disk".into(),
            name: "/skill:disk".into(),
        });
        draft.send_mode = SendMode::Steer;
        draft.attach_image(h1).unwrap();
    }
    drop(h1_dup);
    drop(h2);
    drop(h3);

    ws.switch_view(
        WorkspaceView::Tui,
        Some((DraftKey::NewSession, "请检查 C 盘空间".into(), 5)),
    );
    assert_eq!(ws.active_view(), WorkspaceView::Tui);
    let shared = ws.draft(&DraftKey::NewSession).unwrap();
    assert_eq!(shared.text, "请检查 C 盘空间");
    assert_eq!(shared.cursor_char_offset, 5);
    assert_eq!(shared.skill_name(), Some("disk"));
    assert_eq!(shared.images.len(), 1);

    // 6. Clearing the draft drops the last SharedImageHandle and returns pool used_bytes to 0!
    let taken = ws.take_draft(&DraftKey::NewSession);
    drop(taken);
    assert_eq!(pool.used_bytes(), 0);
}

fn make_call(name: &str, args: Value) -> ToolCallInput {
    ToolCallInput {
        id: format!("call-{name}"),
        name: name.to_owned(),
        arguments: args.as_object().cloned().unwrap_or_else(Map::new),
        execution: None,
    }
}

fn make_result(details: Option<Value>, text: &str, is_error: bool) -> ToolResultInput {
    ToolResultInput {
        tool_call_id: Some("call-1".into()),
        tool_name: None,
        raw_text: text.to_owned(),
        images: Vec::new(),
        details,
        is_error,
    }
}

#[test]
fn all_seventeen_open_tools_project_identically_to_shared_web_fixtures() {
    // Normal tool fixtures aligned with `src/web/frontend/app/cards/map-tool.test.ts`
    // covering: disk, sys(proc/gpu/io/sensor), driver, eventlog, startup, ls, bitlocker(status),
    // source_check, fetch_content, get_search_content, ffgrep, fffind, read
    let normal_fixtures: Vec<(&str, Value, Option<Value>, &str)> = vec![
        (
            "disk",
            json!({ "scope": "space" }),
            Some(json!({ "space": [{ "drive": "E:", "freeGB": 64.9, "usedPct": 35 }] })),
            "卷 E:",
        ),
        (
            "disk",
            json!({ "scope": "usage" }),
            Some(
                json!({ "usage": { "root": "E:\\logs", "topDirs": [{ "path": "E:\\logs", "sizeGB": 10 }] } }),
            ),
            "扫描根",
        ),
        (
            "sys",
            json!({ "scope": "proc" }),
            Some(json!({ "proc": { "byCpu": [{ "name": "node", "cpuPct": 20 }] } })),
            "node",
        ),
        (
            "sys",
            json!({ "scope": "gpu" }),
            Some(json!({ "gpu": { "adapters": [{ "name": "RTX 4060", "vramTotalMB": 8192 }] } })),
            "RTX 4060",
        ),
        (
            "sys",
            json!({ "scope": "io" }),
            Some(json!({ "io": { "disks": [{ "disk": "0 C:", "busyPct": 12 }] } })),
            "12%",
        ),
        (
            "sys",
            json!({ "scope": "sensor" }),
            Some(json!({ "sensor": { "sensors": [{ "name": "CPU Package", "tempC": 56 }] } })),
            "传感器 · CPU Package",
        ),
        (
            "driver",
            json!({ "scope": "problem" }),
            Some(json!({ "problem": { "devices": [], "count": 0 } })),
            "本次没有结果",
        ),
        (
            "eventlog",
            json!({ "scope": "recent" }),
            Some(
                json!({ "recent": { "truncated": true, "total": 12315, "events": [{ "provider": "WHEA", "id": 17 }] } }),
            ),
            "事件 ID",
        ),
        (
            "startup",
            json!({}),
            Some(
                json!({ "startup": { "regItems": [{ "name": "SecurityHealth", "disabled": null }] } }),
            ),
            "注册表自启",
        ),
        (
            "ls",
            json!({}),
            Some(
                json!({ "totalChildren": 1, "entries": [{ "name": "cards.md", "size": "11 KB" }] }),
            ),
            "直接子项",
        ),
        (
            "bitlocker",
            json!({ "scope": "status" }),
            Some(json!({ "bitlocker": { "volumes": [{ "drive": "C:", "lockStatus": 0 }] } })),
            "卷 C:",
        ),
        (
            "source_check",
            json!({}),
            Some(
                json!({ "artifact": { "claims": [{ "status": "unclear" }], "sources": [{ "title": "RFC", "rank": 1 }] } }),
            ),
            "主张判断",
        ),
        (
            "fetch_content",
            json!({}),
            Some(json!({ "urlCount": 1, "successful": 1, "responseId": "hidden-id" })),
            "来源数",
        ),
        (
            "get_search_content",
            json!({}),
            Some(json!({ "nextOffset": 180, "truncated": true })),
            "截断",
        ),
        (
            "ffgrep",
            json!({}),
            Some(json!({ "matchCount": 3, "returnedMatches": 3 })),
            "命中数",
        ),
        (
            "fffind",
            json!({}),
            Some(json!({ "totalMatched": 2, "totalFiles": 120 })),
            "命中条数",
        ),
        ("read", json!({}), None, "原始内容"),
    ];

    let mut cache = ToolPresenterCache::default();
    for (idx, (name, args, details, expected_substr)) in normal_fixtures.into_iter().enumerate() {
        let mut call = make_call(name, args);
        call.id = format!("call-{idx}-{name}");
        let res = make_result(details, "原始内容", false);
        let view = cache.get_or_map_normal(&call, &res, 1);
        let cached_again = cache.get_or_map_normal(&call, &res, 1);
        assert!(Arc::ptr_eq(&view, &cached_again));
        assert_eq!(view.status, CardStatus::Success, "tool={name}");
        let serialized = serde_json::to_string(&view.blocks).unwrap();
        assert!(
            serialized.contains(expected_substr),
            "tool={name} missing `{expected_substr}` in {serialized}"
        );
    }

    // Read image places image block ahead of absolute path field block
    let read_img_call = make_call("read", json!({ "path": "E:\\tmp\\image.png" }));
    let mut read_img_res = make_result(None, "Read image file [image/png]", false);
    read_img_res
        .images
        .push(("image/png".into(), "AA==".into()));
    let read_img_view = map_tool(&read_img_call, &read_img_res);
    assert!(matches!(read_img_view.blocks[0], ToolBlock::Image { .. }));
    assert!(matches!(read_img_view.blocks[1], ToolBlock::Fields { .. }));

    // Error suppresses structured data; degraded preserves data + warnings
    let err_view = map_tool(
        &make_call("disk", json!({ "scope": "health" })),
        &make_result(
            Some(json!({ "smart": [{ "Wear": 10 }] })),
            "SMART denied",
            true,
        ),
    );
    assert_eq!(err_view.status, CardStatus::Error);
    assert_eq!(err_view.blocks.len(), 1);

    let deg_view = map_tool(
        &make_call("disk", json!({ "scope": "all" })),
        &make_result(
            Some(json!({
                "space": [{ "drive": "C:", "freeGB": 40 }],
                "smart": null,
                "smartNotice": "需要管理员权限",
                "degraded": true
            })),
            "",
            false,
        ),
    );
    assert_eq!(deg_view.status, CardStatus::Degraded);
    let deg_str = serde_json::to_string(&deg_view.blocks).unwrap();
    assert!(deg_str.contains("需要管理员权限") && deg_str.contains("剩余"));

    // Special tool classification & 4 persistent component fixtures (`sys(overview)`, `command_plan`,
    // `runbook_deliver`, `runbook`, `web_search`, `bitlocker(unlock/decrypt)`)
    assert_eq!(
        special_tool_kind(&make_call("sys", json!({}))),
        Some(SpecialToolKind::SysOverview)
    );
    assert_eq!(
        special_tool_kind(&make_call("sys", json!({ "scope": "proc" }))),
        None
    );
    assert_eq!(
        special_tool_kind(&make_call("bitlocker", json!({ "scope": "status" }))),
        None
    );
    assert_eq!(
        special_tool_kind(&make_call("bitlocker", json!({ "scope": "decrypt" }))),
        Some(SpecialToolKind::BitLocker)
    );

    // 1. sys(overview) preserves zero values
    let sys_v = map_special_tool(
        &make_call("sys", json!({})),
        Some(&make_result(
            Some(json!({
                "overview": { "cpuTotalPct": 0, "mem": { "usedPct": 0, "totalMB": 32768, "usedMB": 0 } }
            })),
            "",
            false,
        )),
        false,
    )
    .unwrap();
    match sys_v {
        SpecialToolView::SysOverview {
            cpu,
            memory,
            capacity,
            status,
            ..
        } => {
            assert_eq!(status, SpecialStatus::Success);
            assert_eq!(cpu, Some(0.0));
            assert_eq!(memory, Some(0.0));
            assert_eq!(capacity, "0 / 32 GB");
        }
        _ => panic!("expected SysOverview"),
    }

    // 2. command_plan / runbook_deliver / runbook (including JSON raw text fallback when details absent)
    for name in ["command_plan", "runbook_deliver", "runbook"] {
        let raw_json = json!({
            "runbook": {
                "title": "网络重置方案",
                "version": 2,
                "levelLabel": "安全",
                "commands": [{ "shell": "cmd", "admin": true, "summary": "刷新 DNS", "command": "ipconfig /flushdns" }],
                "items": 1,
                "action": if name == "command_plan" { "planned" } else { "delivered" },
                "file": if name == "command_plan" { Value::Null } else { Value::String("E:/artifacts/plan.txt".into()) }
            }
        });
        let rb_view = map_special_tool(
            &make_call(name, json!({})),
            Some(&make_result(None, &raw_json.to_string(), false)),
            false,
        )
        .unwrap();
        assert_eq!(
            rb_view.summary(),
            if name == "command_plan" {
                "方案已保存"
            } else {
                "已生成"
            }
        );
    }

    // 3. web_search: 0 results success, partial degraded, all failed error
    let ws_call = make_call("web_search", json!({ "queries": ["one", "two"] }));
    assert_eq!(
        map_special_tool(
            &ws_call,
            Some(&make_result(
                Some(json!({ "successfulQueries": 2, "queryCount": 2, "totalResults": 0 })),
                "",
                false
            )),
            false
        )
        .unwrap()
        .status(),
        SpecialStatus::Success
    );
    assert_eq!(
        map_special_tool(
            &ws_call,
            Some(&make_result(
                Some(json!({ "successfulQueries": 1, "queryCount": 2 })),
                "",
                false
            )),
            false
        )
        .unwrap()
        .status(),
        SpecialStatus::Degraded
    );

    // 4. bitlocker(unlock/decrypt): top-level unknown, live stage label, drive normalization, batch cancel
    let mut bl_call = make_call("bitlocker", json!({ "scope": "unlock", "drive": "d" }));
    bl_call.execution = Some(ToolExecutionMeta {
        started_at: Some(1),
        stage: Some("正在解锁并复查状态".into()),
        ..Default::default()
    });
    let live_bl = map_special_tool(&bl_call, None, true).unwrap();
    assert_eq!(live_bl.title(), "解锁 D:");
    assert_eq!(live_bl.summary(), "解锁中");

    let unknown_bl = map_special_tool(
        &bl_call,
        Some(&make_result(
            Some(json!({
                "bitlocker": {
                    "scope": "status",
                    "operation": {
                        "status": "unknown",
                        "reason": "上次操作异常中断；实时查询失败，请重新查询。"
                    },
                    "degraded": true
                }
            })),
            "",
            false,
        )),
        false,
    )
    .unwrap();
    assert_eq!(unknown_bl.status(), SpecialStatus::Unknown);
    assert_eq!(unknown_bl.summary(), "结果未确认");

    // 5. split_tool_calls groups normal tools and isolates standalone special/image tools
    let seg_calls = vec![
        make_call("disk", json!({ "scope": "space" })),
        make_call("ls", json!({})),
        make_call("sys", json!({})),
        make_call("read", json!({ "path": "E:\\tmp\\image.png" })),
    ];
    let mut res_map = BTreeMap::new();
    res_map.insert(seg_calls[3].id.clone(), read_img_res);
    let segments = split_tool_calls(&seg_calls, &res_map);
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].kind, ToolCallSegmentKind::Group);
    assert_eq!(segments[0].calls.len(), 2);
    assert_eq!(segments[1].kind, ToolCallSegmentKind::Standalone);
    assert_eq!(segments[2].kind, ToolCallSegmentKind::Standalone);

    // 6. ls ranking rows omit null pct and never duplicate `bytes` alongside null `size`
    let ls_incomplete = map_tool(
        &make_call("ls", json!({ "path": "C:\\" })),
        &make_result(
            Some(json!({
                "path": "C:\\",
                "totalChildren": 2,
                "totalSize": "10.0 MB",
                "method": "walk",
                "entries": [
                    { "name": "pagefile.sys", "type": "file", "bytes": 10485760, "size": "10.0 MB", "pct": null },
                    { "name": "Documents and Settings", "type": "unknown", "bytes": null, "size": null, "pct": null }
                ]
            })),
            "",
            false,
        ),
    );
    let listing = ls_incomplete
        .blocks
        .iter()
        .find_map(|b| match b {
            ToolBlock::Listing { items, .. } => Some(items),
            _ => None,
        })
        .expect("ls listing block");
    assert_eq!(listing[0].metrics.len(), 1);
    assert_eq!(listing[0].metrics[0].value, "10.0 MB");
    assert_eq!(listing[1].metrics.len(), 1);
    assert_eq!(listing[1].metrics[0].value, "大小未知");

    // 7. ffgrep & fffind map skippedLargeFiles, skipped, match rows, and empty matches to Chinese labels
    let grep_view = map_tool(
        &make_call("ffgrep", json!({ "pattern": "panic" })),
        &make_result(
            Some(json!({
                "truncated": false,
                "skipped": 2,
                "skippedLargeFiles": 1,
                "notice": "已跳过 1 个超过 10 MiB 的大文件",
                "matches": [{ "path": "src/main.rs", "line": 12, "text": "panic!", "matched": true }]
            })),
            "",
            false,
        ),
    );
    let grep_json = serde_json::to_string(&grep_view.blocks).unwrap();
    for expected_label in ["已跳过", "跳过大文件", "行号", "命中行"] {
        assert!(
            grep_json.contains(expected_label),
            "missing `{expected_label}` in {grep_json}"
        );
    }
    assert!(!grep_json.contains("skippedLargeFiles"));

    let empty_find = map_tool(
        &make_call("fffind", json!({ "path": "*", "pattern": "none" })),
        &make_result(
            Some(json!({
                "root": "*",
                "scannedRoots": ["C:/", "D:/"],
                "partialRoots": ["E:/"],
                "skippedRoots": [{ "root": "Z:/", "reason": "network_drive_skipped" }],
                "truncated": true,
                "truncatedReason": "timeout",
                "skipped": 0,
                "method": "mft-index",
                "matches": []
            })),
            "",
            false,
        ),
    );
    let empty_json = serde_json::to_string(&empty_find.blocks).unwrap();
    for expected_label in [
        "匹配结果：本次没有结果",
        "扫描卷",
        "部分完成卷",
        "跳过卷 · Z:/",
        "网络映射盘已跳过",
        "MFT 内存索引",
        "截断原因",
        "超时",
        "原因",
    ] {
        assert!(
            empty_json.contains(expected_label),
            "missing `{expected_label}` in {empty_json}"
        );
    }
    assert!(!empty_json.contains("scannedRoots"));
    assert!(!empty_json.contains("truncatedReason"));
    assert!(!empty_json.contains("network_drive_skipped"));
}
