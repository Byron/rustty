//! Image admission is shared by all panes, matching the C server's global LRU.
#[cfg(windows)]
#[path = "../../../conformance/remote_reference.rs"]
mod remote_reference;
use serde_json::json;
#[cfg(unix)]
use std::{path::PathBuf, process::Command};
use tmuxx_terminal::{Effect, ImageQuota, Terminal};
fn control(control: &str, payload: &str) -> Vec<u8> {
    format!("\x1b_G{control};{payload}\x1b\\").into_bytes()
}
fn upload(id: u32) -> Vec<u8> {
    control(&format!("a=t,i={id},f=32,s=1,v=1"), "/wAA/w==")
}
fn place(id: u32) -> Vec<u8> {
    control(&format!("a=p,i={id},C=1"), "")
}
fn compare(name: &str, count: usize, bytes: usize, steps: Vec<(usize, Vec<u8>)>) {
    #[cfg(windows)]
    let (expected, fixture): (Vec<Vec<u8>>, String) = {
        let remote = remote_reference::RemoteReference::new();
        let output = remote.observe("image_quota", &serde_json::to_vec(&json!({"count":count,"bytes":bytes,"steps":steps.iter().map(|(pane,feed)|json!({"pane":pane,"feed":feed})).collect::<Vec<_>>()})).unwrap());
        assert!(
            output.status.success(),
            "remote C quota observation failed: {output:?}"
        );
        (
            serde_json::from_slice(&output.stdout).unwrap(),
            format!("remote C quota fixture {name}"),
        )
    };
    #[cfg(unix)]
    let (expected, fixture, directory): (Vec<Vec<u8>>, String, PathBuf) = {
        let directory = PathBuf::from(format!(
            "/tmp/txquota-fixture-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let fixture = directory.join("fixture.json");
        let output = directory.join("result.json");
        std::fs::write(&fixture,serde_json::to_vec(&json!({"count":count,"bytes":bytes,"steps":steps.iter().map(|(pane,feed)|json!({"pane":pane,"feed":feed})).collect::<Vec<_>>()})).unwrap()).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_owned();
        let status = Command::new("python3")
            .arg("-B")
            .arg(root.join("tmuxx/conformance/image_quota.py"))
            .arg(std::env::var_os("TMUXX_C_TMUX").expect("set TMUXX_C_TMUX"))
            .arg(&fixture)
            .arg(&output)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "C quota observation failed: {}",
            fixture.display()
        );
        (
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap(),
            fixture.display().to_string(),
            directory,
        )
    };
    let quota = ImageQuota::default();
    quota.set_limits(count, bytes);
    let mut panes = [Terminal::new(40, 16, 2000), Terminal::new(40, 16, 2000)];
    for terminal in &mut panes {
        terminal.set_image_quota(quota.clone());
    }
    let mut failures = Vec::new();
    for (index, ((pane, bytes), expected)) in steps.into_iter().zip(expected).enumerate() {
        let actual = panes[pane]
            .feed(&bytes)
            .into_iter()
            .filter_map(|e| {
                if let Effect::Write(b) = e {
                    Some(b)
                } else {
                    None
                }
            })
            .flatten()
            .collect::<Vec<_>>();
        for terminal in &mut panes {
            terminal.prune_images();
        }
        if actual != expected {
            failures.push(format!(
                "{name} step{index}: Rust {:?} C {:?}",
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&expected)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}; fixture {}",
        failures.join("\n"),
        fixture
    );
    #[cfg(unix)]
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
#[ignore = "requires isolated C server with images and TMUXX_C_TMUX"]
fn global_image_sources_placements_queries_lru_and_bytes_match_c() {
    compare(
        "anonymous-address",
        10,
        1024,
        vec![
            (0, control("a=T,f=32,s=1,v=1,C=1", "/wAA/w==")),
            (0, place(u32::MAX)),
            (0, place(1)),
        ],
    );
    compare(
        "anonymous-lifetime",
        2,
        1024,
        vec![
            (0, control("a=T,f=32,s=1,v=1,C=1", "/wAA/w==")),
            (0, b"\x1b[2J\x1b[3J".to_vec()),
            (0, upload(1)),
            (1, upload(2)),
            (0, place(1)),
        ],
    );
    compare(
        "anonymous",
        2,
        1024,
        vec![
            (0, upload(1)),
            (1, control("a=t,f=32,s=1,v=1", "/wAA/w==")),
            (1, upload(2)),
            (0, place(1)),
        ],
    );
    compare(
        "sources",
        1,
        1024,
        vec![(0, upload(1)), (1, upload(2)), (0, place(1))],
    );
    compare(
        "placements",
        2,
        1024,
        vec![
            (0, upload(1)),
            (0, place(1)),
            (1, upload(2)),
            (0, place(1)),
            (1, place(2)),
        ],
    );
    compare(
        "lru",
        3,
        1024,
        vec![
            (0, upload(1)),
            (1, upload(2)),
            (0, place(1)),
            (1, upload(3)),
            (1, place(2)),
            (0, place(1)),
        ],
    );
    compare(
        "bytes",
        100,
        8,
        vec![
            (0, upload(1)),
            (1, upload(2)),
            (0, place(1)),
            (1, upload(3)),
            (1, place(2)),
            (0, place(1)),
        ],
    );
    compare(
        "query",
        1,
        1024,
        vec![
            (0, upload(1)),
            (1, control("a=q,i=2,f=32,s=1,v=1", "/wAA/w==")),
            (0, place(1)),
        ],
    );
    compare(
        "protected",
        1,
        1024,
        vec![(0, upload(1)), (0, place(1)), (0, place(1))],
    );
    compare(
        "replacement",
        2,
        1024,
        vec![
            (0, upload(1)),
            (
                0,
                control("a=T,i=1,f=32,s=1,v=1,c=10000,r=10000", "AP8A/w=="),
            ),
            (0, place(1)),
        ],
    );
}
#[test]
#[ignore = "requires isolated C server with images and TMUXX_C_TMUX"]
fn image_map_lifecycle_across_alternate_screen_and_ris_matches_c() {
    compare(
        "alternate",
        4,
        1024,
        vec![
            (0, upload(1)),
            (0, b"\x1b[?1049h".to_vec()),
            (0, place(1)),
            (0, upload(2)),
            (0, b"\x1b[?1049l".to_vec()),
            (0, place(2)),
            (0, b"\x1bc".to_vec()),
            (0, place(1)),
            (0, place(2)),
        ],
    );
}

#[test]
#[ignore = "requires isolated image-enabled C server and TMUXX_C_TMUX"]
fn sixel_direct_source_quota_overlap_and_text_release_match_c() {
    let sixel = b"\x1b[H\x1bP9;1q\"1;1;1;1#0;2;100;0;0@\x1b\\".to_vec();
    compare(
        "sixel-single-object",
        2,
        1024,
        vec![(0, upload(1)), (1, sixel.clone()), (0, place(1))],
    );
    let mut steps = vec![(0, upload(1))];
    steps.extend((0..20).map(|_| (1, sixel.clone())));
    steps.push((0, place(1)));
    compare("sixel-overlap-releases", 3, 1024, steps);
    compare(
        "sixel-text-releases",
        2,
        1024,
        vec![
            (0, upload(1)),
            (1, sixel),
            (1, b"\x1b[HX".to_vec()),
            (1, upload(2)),
            (0, place(1)),
        ],
    );
}
