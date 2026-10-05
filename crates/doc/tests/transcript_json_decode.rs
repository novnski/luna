//! JSON-boundary acceptance, specified before the optimized decoder.
//! Failures: changed Reset-before-Delta precedence; malformed Reset losing a
//! valid Delta fallback; missing versus null defaults; unknown fields or part
//! variants; lost context/replay data; reordered append/upsert/remove effects;
//! large histories cloning through serde's flatten/untagged intermediate tree.

use luna_doc::{SessionMessageEntry, TranscriptUpdate, apply_transcript_frame};
use serde_json::{Value, json};
use std::time::Instant;

fn entry(id: &str) -> Value {
    json!({"id":id,"role":"assistant","createdAt":42,"deviceId":"fixture",
    "parts":[
        {"kind":"text","id":"text","text":"Mélange 🌓"},
        {"kind":"reasoning","id":"reason","text":"Reasoning"},
        {"kind":"image","id":"image","path":"/fixture/image.png","name":"image","mimeType":"image/png"},
        {"kind":"input","id":"input","requestId":"request","questions":[],"resolved":true},
        {"kind":"asyncQuestion","id":"async","questions":[],"resolved":false},
        {"kind":"error","id":"error","message":"fixture error"},
        {"kind":"tool","id":"tool","call":{"kind":"exec","command":"fixture"},
         "resolved":true,"output":"summary","outputRef":"fixture/tool","outputBytes":262144,
         "subagentRef":"child","subagentStatus":"done",
         "subagentActivity":{"result":"result","outputTokens":7,"summary":"nested"}}
    ]})
}

fn compare(value: &Value) -> bool {
    // The derived implementation remains available as the compatibility oracle.
    let original = serde_json::from_value::<TranscriptUpdate>(value.clone());
    let optimized = TranscriptUpdate::from_json_value(value.clone());
    assert_eq!(original.is_ok(), optimized.is_ok(), "acceptance: {value}");
    match (original, optimized) {
        (Ok(original), Ok(optimized)) => {
            assert_eq!(
                serde_json::to_value(original).unwrap(),
                serde_json::to_value(optimized).unwrap()
            );
            true
        }
        _ => false,
    }
}

#[test]
fn wire_frames_preserve_decoder_and_application_semantics() {
    let reset = json!({"reset":[entry("one")],
        "contextUsage":{"tokens":111,"window":4096},
        "contextControl":luna_proto::ContextControlState::default(),
        "replayBaseline":{"entries":{"one":{"text":12,"tool":0}}}});
    let delta = json!({"count":1,"append":[],"remove":[],"upsert":[]});
    let mut cases = vec![
        reset.clone(),
        delta.clone(),
        json!({"reset":[]}),
        json!({"count":0}),
        json!({"reset":[],"count":"ignored","append":null}),
        json!({"reset":null,"count":0}),
        json!({"reset":[{}],"count":0}),
        json!({"reset":[{}]}),
        json!({"reset":null}),
        json!({}),
        Value::Null,
        json!([]),
        json!(0),
        json!("frame"),
    ];
    for base in [&reset, &delta] {
        for key in [
            "contextUsage",
            "contextControl",
            "providerNotice",
            "replayBaseline",
            "reset",
            "upsert",
            "append",
            "remove",
            "count",
            "unknown",
        ] {
            for value in [
                Value::Null,
                json!(false),
                json!(-1),
                json!(0),
                json!("bad"),
                json!([]),
                json!({}),
                json!([{}]),
            ] {
                let mut frame = base.clone();
                frame[key] = value;
                cases.push(frame);
            }
            let mut frame = base.clone();
            frame.as_object_mut().unwrap().remove(key);
            cases.push(frame);
        }
    }
    for key in [
        "id",
        "role",
        "parts",
        "createdAt",
        "deviceId",
        "status",
        "continuationOf",
    ] {
        for value in [
            Value::Null,
            json!(false),
            json!("unknown"),
            json!([]),
            json!({}),
        ] {
            let mut frame = reset.clone();
            frame["reset"][0][key] = value;
            cases.push(frame.clone());
            frame["count"] = json!(0); // A bad Reset can still be a valid Delta.
            cases.push(frame);
        }
    }
    let accepted = cases.iter().filter(|case| compare(case)).count();
    assert!(compare(&reset));
    let mut old_entries = Vec::new();
    let mut new_entries = Vec::new();
    let mut changed = entry("one");
    changed["parts"][0]["text"] = json!("changed");
    let frames = vec![
        reset,
        json!({"count":1,"append":[{"entry":"one","part":"text","text":"!","len":"Mélange 🌓!".len()}]}),
        json!({"count":2,"upsert":[{"after":"one","entry":entry("two")}]}),
        json!({"count":2,"upsert":[{"after":null,"entry":changed}]}),
        json!({"count":1,"remove":["two"]}),
        json!({"reset":[],"contextUsage":null}),
        json!({"count":0,"append":[{"entry":"missing","part":"text","text":"!","len":1}]}),
    ];
    for frame in &frames {
        let old: TranscriptUpdate = serde_json::from_value(frame.clone()).unwrap();
        let new = TranscriptUpdate::from_json_value(frame.clone()).unwrap();
        assert_eq!(
            apply_transcript_frame(&mut old_entries, old.frame).is_ok(),
            apply_transcript_frame(&mut new_entries, new.frame).is_ok()
        );
        assert_eq!(old_entries, new_entries);
    }
    let mut timings = Vec::new();
    for count in [1200, 4200] {
        let entries: Vec<SessionMessageEntry> = (0..count)
            .map(|i| serde_json::from_value(entry(&format!("entry-{i}"))).unwrap())
            .collect();
        let frame = json!({"reset":entries,"replayBaseline":luna_doc::TranscriptBaseline::capture(&entries)});
        assert!(compare(&frame));
        let mut old_ms = Vec::new();
        let mut new_ms = Vec::new();
        for _ in 0..5 {
            let value = frame.clone();
            let now = Instant::now();
            let old: TranscriptUpdate = serde_json::from_value(value).unwrap();
            old_ms.push(now.elapsed().as_secs_f64() * 1000.0);
            let value = frame.clone();
            let now = Instant::now();
            let new = TranscriptUpdate::from_json_value(value).unwrap();
            new_ms.push(now.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(
                serde_json::to_value(old).unwrap(),
                serde_json::to_value(new).unwrap()
            );
        }
        timings.push(json!({"entries":count,"derivedMs":old_ms,"directMs":new_ms}));
    }
    if let Some(path) = std::env::var_os("LUNA_PERF_EVIDENCE") {
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            std::path::PathBuf::from(path).join("decode-parity.json"),
            serde_json::to_vec_pretty(
                &json!({"passed":true,"cases":cases.len(),"accepted":accepted,
                "appliedFrames":frames.len(),"timings":timings}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}
