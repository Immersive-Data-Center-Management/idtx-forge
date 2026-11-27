use egui::Ui;

use crate::inspector::{
    InspectorContext, InspectorContribution, PropertyClaimSet, widgets::attribute_row,
};

const CAMERA_ATTRS: &[&str] = &[
    "focalLength",
    "horizontalAperture",
    "verticalAperture",
    "horizontalApertureOffset",
    "verticalApertureOffset",
    "clippingRange",
    "fStop",
    "focusDistance",
    "projection",
    "shutter:open",
    "shutter:close",
    "exposure",
    "stereoRole",
];

/// "Camera" section: `UsdGeomCamera` attributes.
#[derive(Debug)]
pub struct CameraPanel;

impl InspectorContribution for CameraPanel {
    fn label(&self) -> &'static str {
        "Camera"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn claim(&self, claims: &mut PropertyClaimSet) {
        for &name in CAMERA_ATTRS {
            claims.claim(name);
        }
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        ctx.type_name() == "Camera"
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        let cam_props: Vec<_> = ctx
            .properties
            .iter()
            .filter(|p| CAMERA_ATTRS.contains(&p.name.as_str()))
            .collect();

        if cam_props.is_empty() {
            ui.label(
                egui::RichText::new("No camera attributes authored")
                    .small()
                    .weak(),
            );
            return;
        }

        for prop in cam_props {
            attribute_row(ui, ctx, prop);
        }
    }
}
