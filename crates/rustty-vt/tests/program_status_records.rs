use rustty_vt::program_status::{Kind, MAX_RECORDS, Records, Report, State};
use serde_json::{Value, json};

fn report(body: &str) -> Report {
    Report::parse(body.as_bytes()).unwrap()
}

#[test]
fn complete_metadata_respects_state_and_replacement() {
    let value = report(
        "state=blocked:id=job:kind=auth:progress=000100:app=worker:title=VGl0bGU:msg=TWVzc2FnZQ",
    );
    assert_eq!(value.kind, Some(Kind::Auth));
    assert_eq!(value.progress, Some(100));
    assert_eq!(
        (&*value.app, &*value.title, &*value.message),
        ("worker", "Title", "Message")
    );
    for raw in ["-1", "101", "1.0", "999999999999999999999", ""] {
        assert_eq!(
            report(&format!("state=working:progress={raw}")).progress,
            None
        );
    }
    for state in ["idle", "done", "error", "clear"] {
        let value = report(&format!("state={state}:kind=auth:progress=10"));
        assert_eq!((value.kind, value.progress), (None, None));
    }
    assert_eq!(report("state=blocked:kind=future:app=bad,name").kind, None);
    assert_eq!(report("state=blocked:kind=future:app=bad,name").app, "");
    assert!(Report::parse(b"process-exited").is_none());
    let mut records = Records::default();
    records.apply(value).unwrap();
    records.apply(report("state=done:id=job")).unwrap();
    assert_eq!(
        records.get(Some("job")),
        Some(&Report {
            state: State::Done,
            id: Some("job".into()),
            ..Default::default()
        })
    );
    assert_eq!(records.activity(), Some(rustty_vt::agent::State::Done));
}

#[test]
fn lifecycle_matches_kitty_production_reference() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../test/rustty/fixtures/kitty-program-status.json"
    ))
    .unwrap();
    let mut records = Records::default();
    for step in fixture["steps"].as_array().unwrap() {
        let changed = match step["action"].as_str().unwrap() {
            "report" => records
                .apply(report(step["body"].as_str().unwrap()))
                .unwrap(),
            "exit" => records.on_process_exit_or_prompt(),
            "seen" => records.on_seen_by_user(),
            _ => unreachable!(),
        };
        assert_eq!(changed, step["changed"].as_bool().unwrap(), "{step}");
        assert_eq!(
            serde_json::to_value(records.query()).unwrap(),
            step["records"],
            "{step}"
        );
        assert_eq!(
            records
                .most_urgent()
                .map(|r| r.id.as_deref().unwrap_or_default()),
            step["urgent"].as_str(),
            "{step}"
        );
        // Checkpointing at any boundary preserves both ordering policies.
        records = serde_json::from_slice(&serde_json::to_vec(&records).unwrap()).unwrap();
    }
}

#[test]
fn independent_change_and_eviction_order_survives_restoration() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../test/rustty/fixtures/kitty-program-status.json"
    ))
    .unwrap();
    let mut records = Records::default();
    for index in 0..MAX_RECORDS {
        records
            .apply(report(&format!("state=working:id=item{index}")))
            .unwrap();
    }
    assert!(!records.apply(report("state=working:id=item0")).unwrap());
    assert_eq!(
        records.most_urgent().unwrap().id.as_deref(),
        Some("item255")
    );
    records = serde_json::from_slice(&serde_json::to_vec(&records).unwrap()).unwrap();
    records.apply(report("state=working:id=item256")).unwrap();
    let ids = records
        .iter()
        .map(|r| r.id.as_ref().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_value(ids).unwrap(),
        fixture["eviction"]["ids"]
    );
    assert_eq!(records.len(), MAX_RECORDS);
    assert_eq!(
        records.most_urgent().unwrap().id.as_deref(),
        fixture["eviction"]["urgent"].as_str()
    );
    assert!(records.get(Some("item0")).is_some());
    assert!(records.get(Some("item1")).is_none());
    assert!(records.clear());
    assert!(!records.clear());
}

#[test]
fn bounded_restoration_rejects_invalid_records_and_orders() {
    let good = json!({"records":[{"state":"working","id":"job"}],"reported":["job"]});
    let _: Records = serde_json::from_value(good.clone()).unwrap();
    let mut bad = Vec::new();
    for (field, value) in [
        ("state", json!("clear")),
        ("id", json!("")),
        ("app", json!("a/b")),
        ("title", json!("x".repeat(193))),
        ("message", json!("x".repeat(2049))),
        ("message", json!("control\n")),
        ("kind", json!("auth")),
        ("progress", json!(101)),
    ] {
        let mut value_state = good.clone();
        value_state["records"][0][field] = value;
        bad.push(value_state);
    }
    bad.push(json!({"records":good["records"],"reported":[]}));
    bad.push(json!({"records":good["records"],"reported":["other"]}));
    bad.push(json!({"records":[good["records"][0],good["records"][0]],"reported":["job","job"]}));
    bad.push(json!({"records":[{"state":"working","id":"a"},{"state":"done","id":"b"}],"reported":["a","a"]}));
    bad.push(json!({"records": vec![json!({"state":"idle"}); MAX_RECORDS + 1],"reported":vec![Value::Null; MAX_RECORDS + 1]}));
    for value in bad {
        assert!(
            serde_json::from_value::<Records>(value.clone()).is_err(),
            "{value}"
        );
    }
    let mut records = Records::default();
    assert!(
        records
            .apply(Report {
                progress: Some(101),
                ..Default::default()
            })
            .is_err()
    );
    assert!(records.is_empty());
}
