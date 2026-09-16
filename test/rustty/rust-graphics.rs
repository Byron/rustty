use rustty_vt::{
    GridPoint, Screen, Terminal,
    graphics::{PlacementId, unicode},
};
use serde_json::{Value, json};

pub fn observe(terminal: &Terminal) -> Value {
    json!({"primary":screen(terminal.primary_screen()),
           "alternate":terminal.alternate_screen().map(screen)})
}

pub fn tick(terminal: &mut Terminal, now_ms: u64) -> Value {
    let before = terminal.graphics().generation;
    let deadline = terminal.tick_graphics(now_ms);
    json!({"next_delay_ms":deadline.map(|next| next.saturating_sub(now_ms)),
        "changed":terminal.graphics().generation != before})
}

pub fn observe_placements(terminal: &Terminal) -> Value {
    let cell = [
        terminal.width_px / u32::from(terminal.cols),
        terminal.height_px / u32::from(terminal.rows),
    ];
    json!({"primary":placements(terminal.primary_screen(), cell),
        "alternate":terminal.alternate_screen().map(|screen| placements(screen, cell)),
        "primary_placeholders":placeholders(terminal.primary_screen(),cell),
        "alternate_placeholders":terminal.alternate_screen().map(|screen| placeholders(screen,cell))})
}

fn placeholders(screen: &Screen, cell: [u32; 2]) -> Vec<Value> {
    screen
        .viewport()
        .flat_map(|row| {
            unicode::placements(screen, row).map(move |p| {
                let target = screen
                    .graphics
                    .placeholder_target(p.image_id, p.placement_id);
                let geometry = screen.graphics.images.get(&p.image_id).ok_or("MissingImage")
                    .and_then(|image| target.ok_or("PlacementMissingPlacement").map(|target| (image,target)))
                    .and_then(|(image,target)| p.geometry(target,image,cell).ok_or("PlacementGridOutOfBounds"));
                json!({"image_id":p.image_id,"placement_id":p.placement_id,
            "anchor":location(screen,GridPoint { row:row.id,col:p.col }),
            "fragment":[p.image_col,p.image_row],"size":[p.width,1],
            "target":target.map(|target| placement_id(target.placement_id)),
            "geometry":geometry.as_ref().ok().map(|g| json!({"offset":g.offset,"source":g.source,"pixels":g.pixels})),
            "geometry_error":geometry.err()})
            })
        })
        .collect()
}

fn placement_id(id: PlacementId) -> Value {
    match id {
        PlacementId::Internal(id) => json!({"internal":true,"id":id}),
        PlacementId::External(id) => json!({"internal":false,"id":id}),
    }
}

fn location(screen: &Screen, point: GridPoint) -> Option<Value> {
    let y = screen.all_rows().position(|row| row.id == point.row)?;
    let relative = |start| y.checked_sub(start).map(|y| [point.col, y]);
    Some(
        json!({"screen":[point.col,y], "active":relative(screen.history_len()),
        "viewport":relative(screen.history_len().saturating_sub(screen.viewport_offset))}),
    )
}

fn placements(screen: &Screen, cell: [u32; 2]) -> Vec<Value> {
    let mut placements: Vec<_> = screen.graphics.placements.iter().collect();
    placements.sort_by_key(|p| (p.image_id, p.placement_id));
    placements.into_iter().map(|p| {
        let image = &screen.graphics.images[&p.image_id];
        let kind = if p.virtual_placement { "virtual" } else if p.parent.is_some() { "relative" } else { "pin" };
        let chain = p.parent.and_then(|_| p.resolve_chain(|key| screen.graphics.placements.iter()
            .find(|p| (p.image_id,p.placement_id) == key))).map(|(root,offset)| {
                json!({"image_id":root.image_id,"placement_id":placement_id(root.placement_id),
                    "anchor":if root.virtual_placement { None } else { location(screen,GridPoint { row:root.row,col:root.col }) },
                    "offset":offset})
            });
        json!({"image_id":p.image_id, "placement_id":placement_id(p.placement_id),
            "location":kind,
            "anchor":if kind == "pin" { location(screen, GridPoint { row:p.row, col:p.col }) } else { None },
            "parent":p.parent.map(|(id,key)| json!({"image_id":id,"placement_id":placement_id(key),"offset":p.parent_offset})),
            "chain":chain,
            "requested_size":[p.columns,p.rows], "requested_source":p.source, "stored_offset":p.offset,
            "z":p.z,"source":p.source_rect(image),"offset":p.cell_offset(cell),
            "pixels":p.pixel_size(image,cell),"grid":p.grid_size(image,cell),
            "rect":p.grid_rect(screen,image,cell).map(|(a,b)| json!({"start":location(screen,a),"end":location(screen,b)}))})
    }).collect()
}

fn screen(screen: &Screen) -> Vec<Value> {
    let mut images: Vec<_> = screen.graphics.images.values().collect();
    images.sort_by_key(|image| image.id);
    images
        .into_iter()
        .map(|image| {
            json!({"id":image.id,"number":image.number,
        "width":image.width,"height":image.height,"pixels":super::hex(&image.pixels),
        "displayed_pixels":super::hex(image.display_pixels()),
        "frames":image.frames.iter().map(|frame| json!({"pixels":super::hex(&frame.pixels),"gap_ms":frame.gap_ms})).collect::<Vec<_>>(),
        "current_frame":image.current_frame,"root_gap_ms":image.root_gap_ms,
        "animation_state":image.animation_state,"max_loops":image.max_loops,
        "completed_loops":image.completed_loops,"frame_shown_at_ms":image.frame_shown_at_ms})
        })
        .collect()
}
