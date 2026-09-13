use rustty_vt::{
    GridPoint, Screen, Terminal,
    graphics::{PlacementId, unicode},
};
use serde_json::{Value, json};

pub fn observe(terminal: &Terminal) -> Value {
    json!({"primary":screen(terminal.primary_screen()),
           "alternate":terminal.alternate_screen().map(screen)})
}

pub fn observe_placements(terminal: &Terminal) -> Value {
    let cell = [
        terminal.width_px / u32::from(terminal.cols),
        terminal.height_px / u32::from(terminal.rows),
    ];
    json!({"primary":placements(terminal.primary_screen(), cell),
        "alternate":terminal.alternate_screen().map(|screen| placements(screen, cell)),
        "primary_placeholders":placeholders(terminal.primary_screen()),
        "alternate_placeholders":terminal.alternate_screen().map(placeholders)})
}

fn placeholders(screen: &Screen) -> Vec<Value> {
    screen
        .viewport()
        .flat_map(|row| {
            unicode::placements(row).map(|p| {
                let target = screen
                    .graphics
                    .placeholder_target(p.image_id, p.placement_id);
                json!({"image_id":p.image_id,"placement_id":p.placement_id,
            "anchor":location(screen,GridPoint { row:row.id,col:p.col }),
            "fragment":[p.image_col,p.image_row],"size":[p.width,1],
            "target":target.map(|target| placement_id(target.placement_id))})
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
        json!({"screen":[point.col,y], "active":relative(screen.history.len()),
        "viewport":relative(screen.history.len().saturating_sub(screen.viewport_offset))}),
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
        "width":image.width,"height":image.height,"pixels":super::hex(&image.pixels)})
        })
        .collect()
}
