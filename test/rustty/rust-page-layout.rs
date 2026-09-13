//! Compile the private production calculator directly into the test adapter.
#[path = "../../crates/rustty-vt/src/page_layout.rs"]
mod page_layout;

use page_layout::{LayoutError, PageCapacity};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Request {
    action: String,
    columns: u16,
    capacity: Option<PageCapacity>,
    exact: bool,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            action: "initial".into(),
            columns: 80,
            capacity: None,
            exact: false,
        }
    }
}

pub fn run(request: &Request) -> Result<Value, &'static str> {
    if !matches!(request.action.as_str(), "initial" | "adjust" | "layout") {
        return Err("InvalidLayoutAction");
    }
    run_inner(request).map_err(|error| match error {
        LayoutError::InvalidDimensions => "InvalidDimensions",
        LayoutError::OutOfMemory => "OutOfMemory",
        LayoutError::RowCountOverflow => "RowCountOverflow",
        LayoutError::PageTooLarge => "PageTooLarge",
        LayoutError::ArithmeticOverflow => "ArithmeticOverflow",
        LayoutError::UnsupportedPlatform => "UnsupportedPlatform",
    })
}

fn run_inner(request: &Request) -> Result<Value, LayoutError> {
    let mut capacity = request.capacity.unwrap_or(PageCapacity::STANDARD);
    capacity.layout()?;
    capacity = match request.action.as_str() {
        "initial" => PageCapacity::initial(request.columns)?,
        "adjust" => capacity.adjust_columns(request.columns)?,
        "layout" => capacity,
        _ => unreachable!("validated layout action"),
    };
    let layout = capacity.layout()?;
    Ok(json!({
        "constants": {
            "page_alignment": page_layout::PAGE_ALIGNMENT,
            "cell_alignment": page_layout::CELL_ALIGNMENT,
            "row_size": page_layout::ROW_SIZE,
            "cell_size": page_layout::CELL_SIZE,
            "style_item_size": page_layout::STYLE_ITEM_SIZE,
            "style_item_alignment": page_layout::STYLE_ITEM_ALIGNMENT,
            "style_set_alignment": page_layout::METADATA_ALIGNMENT,
            "hyperlink_item_size": page_layout::HYPERLINK_ITEM_SIZE,
            "hyperlink_item_alignment": page_layout::METADATA_ALIGNMENT,
            "hyperlink_set_alignment": page_layout::METADATA_ALIGNMENT,
            "metadata_alignment": page_layout::METADATA_ALIGNMENT,
            "standard_capacity": PageCapacity::STANDARD,
            "standard_bytes": PageCapacity::STANDARD.layout()?.total_size,
        },
        "layout": layout,
        "metadata": capacity.metadata()?,
        "allocation_bytes": layout.allocation_bytes(request.exact),
        "pooled": layout.pooled(request.exact),
    }))
}
