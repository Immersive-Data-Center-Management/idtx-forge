//! Execute the shipped Rust menu against ordinary hydrated USD entities.
use super::*;

#[test]
#[ignore = "requires OpenUSD, headless GPU and wasm32-unknown-unknown"]
fn shipped_menu_attachment_uses_session_local_units_under_transformed_parents() {
    idtx_usd_extension::install().unwrap();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let artifact = idtx_behavior_build::build_behavior(
        &workspace.join("examples/widgets/menu.rs"),
        &idtx_behavior_build::BuildOptions::new(
            dir.path().join("modules"),
            workspace.join("crates/idtx-behavior-sdk"),
        ),
    )
    .unwrap();
    let example = workspace.join("examples/widgets/radial_menu.usda");
    for (case, units, up, top_level) in [
        (0, 1., "Y", false),
        (1, 0.01, "Z", false),
        (2, 0.01, "Z", true),
    ] {
        let path = dir.path().join(format!("attachment{case}.usda"));
        let panel = format!(
            r#"
    def "Menu" (prepend references = @{}@</Menu>) {{
        asset idtx:behavior:module = @{}@
        string idtx:behavior:context = '{{"container":"Items","pointer":1,"priority":100,"open":"menu.open","navigate":"menu.navigate","confirm":"menu.confirm","cancel":"menu.cancel","next":"menu.next","previous":"menu.previous","capture":["menu.open","menu.confirm","menu.cancel"],"actions":[]}}'
        rel controller = </World/Player/Controller>
        rel attachmentOffset = </World/Offsets/Offset>
    }}
"#,
            example.to_string_lossy().replace('\\', "/"),
            artifact.wasm_path.to_string_lossy().replace('\\', "/")
        );
        let text = format!(
            r#"#usda 1.0
(
    metersPerUnit = {units}
    upAxis = "{up}"
)
def Xform "World" {{
    def Xform "Player" {{
        double3 xformOp:translate = (50, 70, 90)
        float xformOp:rotateY = 30
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateY"]
        def Xform "Controller" {{
            double3 xformOp:translate = (10, 20, 30)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }}
    }}
    def Xform "Offsets" {{
        double3 xformOp:translate = (500, 600, 700)
        float xformOp:rotateX = 15
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
        def Xform "Offset" {{
            double3 xformOp:translate = (5, 10, -20)
            float xformOp:rotateZ = 20
            uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateZ"]
        }}
    }}
    def Xform "MenuParent" {{
        double3 xformOp:translate = (100, 200, 300)
        float xformOp:rotateZ = 45
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateZ"]
        {}
    }}
}}
{}
"#,
            if top_level { "" } else { &panel },
            if top_level { &panel } else { "" }
        );
        std::fs::write(&path, text).unwrap();
        let (device, queue) = pollster::block_on(async {
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
                .unwrap()
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .unwrap()
        });
        let mut session = StudioSession::open_with_gpu(
            &path,
            device,
            queue,
            [32, 32],
            TextureDecodeConfig::FULL_RESOLUTION,
            RenderWorldUnitScale::METRE,
        )
        .unwrap();
        session.set_behavior_input_map(
            InputMap::parse(include_str!(
                "../../idtx-assets/built-in/projects/input_map.toml"
            ))
            .unwrap(),
        );
        assert_eq!(session.attach_authored_behaviors(|_| None), 1);
        let panel_path = if top_level {
            "/Menu"
        } else {
            "/World/MenuParent/Menu"
        };
        let entity = session.hydrator.entity_for_path(panel_path).unwrap();
        assert!(
            session
                .world()
                .get::<idtx_behavior::ecs::Transform>(entity)
                .is_none()
        );
        let root_before = session.root_layer_snapshot().unwrap();
        session.set_play_state(PlayState::Playing);
        session.tick_behaviors(0.016);
        session.press_input("Key/M");
        session.tick_behaviors(0.016);
        session.release_input("Key/M");
        for x in [10., 35., 60.] {
            session
                .service
                .author_session_transform(
                    "/World/Player/Controller",
                    Mat4::from_translation(glam::Vec3::new(x, 20., 30.))
                        .to_cols_array()
                        .map(f64::from),
                )
                .unwrap();
            session.tick_behaviors(0.016);
            let controller = session
                .hydrator
                .entity_for_path("/World/Player/Controller")
                .unwrap();
            let offset = session
                .service
                .prim_at_path("/World/Offsets/Offset")
                .unwrap();
            let offset = Mat4::from_cols_array(
                &session
                    .service
                    .get_local_transform_f64(offset)
                    .unwrap()
                    .map(|v| v as f32),
            );
            let expected = session
                .world()
                .get::<WorldTransform>(controller)
                .unwrap()
                .matrix
                * offset;
            let actual = session
                .world()
                .get::<WorldTransform>(entity)
                .unwrap()
                .matrix;
            assert!(
                actual.abs_diff_eq(expected, 0.002),
                "case {case}: attachment {actual:?} != {expected:?}"
            );
            assert_eq!(
                session
                    .behaviors
                    .diagnostics(
                        idtx_behavior::entity_to_wire(entity),
                        "idtx-radial-menu-example"
                    )
                    .unwrap()
                    .2,
                0
            );
        }
        assert_eq!(
            session.root_layer_snapshot().unwrap(),
            root_before,
            "attachment must stay in the session layer"
        );
        assert!(
            session
                .world()
                .get::<idtx_behavior::ecs::Transform>(entity)
                .is_none()
        );
    }
}
