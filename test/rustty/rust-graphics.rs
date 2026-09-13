use rustty_vt::{Screen, Terminal};
use serde_json::{Value, json};

pub fn observe(terminal: &Terminal) -> Value {
    json!({"primary":screen(terminal.primary_screen()),
           "alternate":terminal.alternate_screen().map(screen)})
}

fn screen(screen: &Screen) -> Vec<Value> {
    let mut images: Vec<_> = screen.graphics.images.values().collect();
    images.sort_by_key(|image| image.id);
    images
        .into_iter()
        .map(|image| {
            json!({"id":image.id,"number":image.number,
        "width":image.width,"height":image.height,"pixels":super::hex(&image.pixels)})
        })
        .collect()
}
