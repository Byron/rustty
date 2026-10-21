use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SnapshotState {
    pub sessions: Vec<SnapshotSession>,
    pub windows: Vec<SnapshotWindow>,
    pub panes: Vec<SnapshotPane>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotSession {
    pub id: u64,
    pub name: String,
    pub active: u64,
    pub windows: BTreeMap<u32, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotWindow {
    pub id: u64,
    pub name: String,
    pub active: u64,
    pub cols: u16,
    pub rows: u16,
    pub panes: Vec<u64>,
    pub layout: String,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub zoomed_pane: Option<u64>,
    #[serde(default)]
    pub visible_layout: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotPane {
    pub id: u64,
    pub window: u64,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    pub cols: u16,
    pub rows: u16,
    pub status: Option<i32>,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub modal: bool,
    #[serde(default)]
    pub modal_capture_keys: bool,
    #[serde(default)]
    pub modal_close_click: bool,
    #[serde(default)]
    pub modal_close_cancel: bool,
    /// Disconnected views retain pane identity while releasing terminal storage.
    #[serde(default)]
    pub terminal: Option<Arc<tmuxx_terminal::CheckpointSnapshot>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{ServerMessage, decode};

    #[test]
    fn checkpoints_and_images_remain_binary_and_restore_through_tagged_messages() {
        let mut terminal = tmuxx_terminal::Terminal::new(40, 6, 2000);
        for line in 0..200 {
            terminal.feed(
                format!("\x1b[38;2;12;34;56m{line}: compiled α module\x1b[0m\r\n").as_bytes(),
            );
        }
        terminal.feed(b"\x1b]7;file://host/remote%20path\x07\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07");
        terminal.feed(b"\x1b_Ga=T,i=7,f=32,s=1,v=1;/wCA/w==\x1b\\");
        terminal.feed(b"\x1b[38;2;");
        let snapshot = Arc::new(terminal.checkpoint_snapshot());
        let mut state: SnapshotState = serde_json::from_value(serde_json::json!({
            "sessions": [{"id":0,"name":"test","active":0,"windows":{"0":0}}],
            "windows": [{"id":0,"name":"shell","active":0,"cols":40,"rows":6,"panes":[0],"layout":""}],
            "panes": [{"id":0,"window":0,"cols":40,"rows":6,"status":null}]
        })).unwrap();
        state.panes[0].terminal = Some(snapshot.clone());
        let copied = state.clone();
        assert!(Arc::ptr_eq(
            copied.panes[0].terminal.as_ref().unwrap(),
            &snapshot
        ));
        let json_bytes = serde_json::to_vec(&state).unwrap().len();
        let binary_bytes = rmp_serde::to_vec_named(&state).unwrap().len();
        let mut wire = Vec::new();
        crate::native::write_snapshot(
            &mut wire,
            "owner",
            1,
            &state,
            crate::native::MAX_SNAPSHOT_BYTES,
        )
        .unwrap();
        eprintln!(
            "200 colored log rows: previous checkpoint JSON={json_bytes} bytes, MessagePack={binary_bytes} bytes, compressed snapshot wire={} bytes",
            wire.len()
        );
        assert!(binary_bytes < json_bytes);
        assert!(wire.len() < binary_bytes);
        let message = ServerMessage::Snapshot {
            instance_id: "owner".into(),
            sequence: 1,
            state,
        };
        let bytes = rmp_serde::to_vec_named(&message).unwrap();
        let checkpoint = snapshot.checkpoint.as_ref().unwrap();
        assert!(
            bytes
                .windows(checkpoint.len())
                .any(|bytes| bytes == checkpoint)
        );
        assert!(bytes.windows(4).any(|bytes| bytes == [255, 0, 128, 255]));
        let ServerMessage::Snapshot { state, .. } = decode(&bytes).unwrap() else {
            panic!("missing snapshot");
        };
        let mut restored = state.panes[0].terminal.as_ref().unwrap().restore().unwrap();
        let capture = tmuxx_terminal::CaptureOptions {
            start: Some(i32::MIN),
            escape_sequences: true,
            ..Default::default()
        };
        assert_eq!(
            restored.capture_with(capture).unwrap(),
            terminal.capture_with(capture).unwrap()
        );
        assert_eq!(restored.cwd(), terminal.cwd());
        assert_eq!(
            restored.snapshot().primary_graphics.images[0]
                .pixels
                .as_slice(),
            [255, 0, 128, 255]
        );
        terminal.feed(b"1;2;3mcontinued");
        restored.feed(b"1;2;3mcontinued");
        assert_eq!(
            restored.capture_with(capture).unwrap(),
            terminal.capture_with(capture).unwrap()
        );
    }
}
