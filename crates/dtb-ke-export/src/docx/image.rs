//! DrawingML picture construction — one floating `<w:drawing>` per header logo.
//!
//! The logos are anchored **relative to the page** (`wp:anchor`, `behindDoc`,
//! `wrapNone`) so they can sit past the text margins — toward and over the page
//! edges — the way the Typst template's `place()`d header art does. The picture
//! subtree mirrors what Word writes for an inserted picture:
//! `wp:anchor → a:graphic → a:graphicData(uri=picture) → pic:pic`, with a
//! `pic:blipFill/a:blip` that references the PNG part and carries an
//! `asvg:svgBlip` extension pointing at the SVG part (Word 2016+ renders the
//! vector, everything else the PNG).

use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;
use ooxmlsdk::schemas::{a, asvg, pic, wp};
use ooxmlsdk::units::CoordinateValue;

/// The `a:ext` URI that marks an SVG blip extension (MS Office, fixed GUID).
const SVG_EXT_URI: &str = "{96DAC541-7B7A-43D3-8B79-37D633B846F1}";
/// The graphic-data URI for a DrawingML picture.
const PICTURE_URI: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";

/// Everything needed to place one logo (offsets are from the **page** top-left,
/// in EMU; a small `offset_x` bleeds the frame toward the left page edge).
pub struct LogoPlacement<'a> {
    /// Relationship id of the PNG image part (`r:embed` on `a:blip`).
    pub png_rid: &'a str,
    /// Relationship id of the SVG image part (`r:embed` on `asvg:svgBlip`).
    pub svg_rid: &'a str,
    /// Frame width / height in EMU.
    pub cx: i64,
    pub cy: i64,
    /// Offset of the frame's left / top edge from the page's left / top edge.
    pub offset_x: i32,
    pub offset_y: i32,
    /// Unique drawing id and a human name (shows in Word's selection pane).
    pub id: u32,
    pub name: &'a str,
    /// Relative Z-order (`wp:anchor/@relativeHeight`).
    pub z: u32,
}

/// A `<w:r>` wrapping the floating drawing for one logo.
pub fn anchored_logo_run(p: &LogoPlacement<'_>) -> w::Run {
    w::Run {
        run_choice: vec![w::RunChoice::Drawing(Box::new(w::Drawing {
            drawing_choice: Some(w::DrawingChoice::Anchor(Box::new(anchor(p)))),
            ..Default::default()
        }))],
        ..Default::default()
    }
}

fn anchor(p: &LogoPlacement<'_>) -> wp::Anchor {
    wp::Anchor {
        distance_from_top: Some(0),
        distance_from_bottom: Some(0),
        distance_from_left: Some(0),
        distance_from_right: Some(0),
        simple_pos: Some(false.into()),
        relative_height: Some(p.z),
        behind_doc: true.into(),
        locked: false.into(),
        layout_in_cell: true.into(),
        allow_overlap: true.into(),
        // `wp:simplePos` is a *required* child element even when unused.
        simple_position: Some(wp::SimplePosition {
            x: CoordinateValue::Emu(0),
            y: CoordinateValue::Emu(0),
        }),
        horizontal_position: Some(Box::new(wp::HorizontalPosition {
            relative_from: wp::HorizontalRelativePositionValues::Page,
            horizontal_position_choice: Some(wp::HorizontalPositionChoice::PositionOffset(
                p.offset_x,
            )),
        })),
        vertical_position: Some(Box::new(wp::VerticalPosition {
            relative_from: wp::VerticalRelativePositionValues::Page,
            vertical_position_choice: Some(wp::VerticalPositionChoice::PositionOffset(p.offset_y)),
        })),
        extent: wp::Extent { cx: p.cx, cy: p.cy },
        anchor_choice: Some(wp::AnchorChoice::WrapNone),
        doc_properties: Some(Box::new(wp::DocProperties {
            id: p.id,
            name: p.name.to_string(),
            ..Default::default()
        })),
        non_visual_graphic_frame_drawing_properties: Some(Box::new(
            wp::NonVisualGraphicFrameDrawingProperties {
                graphic_frame_locks: Some(Box::new(a::GraphicFrameLocks {
                    no_change_aspect: Some(true.into()),
                    ..Default::default()
                })),
                ..Default::default()
            },
        )),
        graphic: Box::new(a::Graphic {
            graphic_data: a::GraphicData {
                uri: PICTURE_URI.to_string(),
                graphic_data_choice: vec![a::GraphicDataChoice::Picture(Box::new(picture(p)))],
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn picture(p: &LogoPlacement<'_>) -> pic::Picture {
    pic::Picture {
        non_visual_picture_properties: Some(Box::new(pic::NonVisualPictureProperties {
            non_visual_drawing_properties: Box::new(pic::NonVisualDrawingProperties {
                id: p.id,
                name: p.name.to_string(),
                ..Default::default()
            }),
            non_visual_picture_drawing_properties: Box::new(
                pic::NonVisualPictureDrawingProperties::default(),
            ),
        })),
        blip_fill: Some(Box::new(pic::BlipFill {
            blip: Some(Box::new(blip(p))),
            blip_fill_choice: Some(pic::BlipFillChoice::Stretch(Box::new(a::Stretch {
                fill_rectangle: Some(a::FillRectangle::default()),
                ..Default::default()
            }))),
            ..Default::default()
        })),
        shape_properties: Some(Box::new(pic::ShapeProperties {
            transform2_d: Some(Box::new(a::Transform2D {
                offset: Some(a::Offset {
                    x: CoordinateValue::Emu(0),
                    y: CoordinateValue::Emu(0),
                }),
                extents: Some(a::Extents {
                    cx: CoordinateValue::Emu(p.cx),
                    cy: CoordinateValue::Emu(p.cy),
                }),
                ..Default::default()
            })),
            shape_properties_choice1: Some(pic::ShapePropertiesChoice::PresetGeometry(Box::new(
                a::PresetGeometry {
                    preset: a::ShapeTypeValues::Rectangle,
                    ..Default::default()
                },
            ))),
            ..Default::default()
        })),
        ..Default::default()
    }
}

fn blip(p: &LogoPlacement<'_>) -> a::Blip {
    a::Blip {
        embed: Some(p.png_rid.to_string()),
        blip_extension_list: Some(a::BlipExtensionList {
            blip_extension: vec![a::BlipExtension {
                uri: SVG_EXT_URI.to_string(),
                blip_extension_choice: Some(a::BlipExtensionChoice::SvgBlip(asvg::SvgBlip {
                    embed: Some(p.svg_rid.to_string()),
                    ..Default::default()
                })),
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
}
