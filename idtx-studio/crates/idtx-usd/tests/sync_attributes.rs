#![cfg(feature = "authoring")]

use idtx_usd::{StageService, SyncAttribute, SyncAttributeValue};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn open(source: &str) -> (Self, StageService) {
        let path = std::env::temp_dir().join(format!(
            "idtx-sync-attributes-{}-{}.usda",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::write(&path, source).unwrap();
        let stage = StageService::open_topology_only(&path).unwrap();
        (Self(path), stage)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn doubles(name: &str, type_name: &str, values: &[f64], is_array: bool) -> SyncAttribute {
    SyncAttribute {
        name: name.into(),
        type_name: type_name.into(),
        value: SyncAttributeValue {
            doubles: values.to_vec(),
            is_array,
            ..Default::default()
        },
    }
}

#[test]
fn typed_defaults_preserve_roles_arrays_integer_boundaries_and_component_order() {
    let (_fixture, mut stage) = Fixture::open("#usda 1.0\ndef Xform \"World\" {}\n");
    let attributes = vec![
        doubles("属性:éclairage", "double", &[1.0], false),
        doubles("color", "color3f", &[0.25, 0.5, 1.0], false),
        doubles("points", "point3f", &[1.0, 2.0, 3.0], true),
        doubles("empty", "float", &[], true),
        doubles(
            "frame",
            "frame4d",
            &(0..16).map(f64::from).collect::<Vec<_>>(),
            false,
        ),
        doubles("rotation", "quatd", &[1.0, 2.0, 3.0, 4.0], false),
        SyncAttribute {
            name: "signed".into(),
            type_name: "int64".into(),
            value: SyncAttributeValue {
                ints: vec![i64::MIN, i64::MAX],
                is_array: true,
                ..Default::default()
            },
        },
        SyncAttribute {
            name: "unsigned".into(),
            type_name: "uint64".into(),
            value: SyncAttributeValue {
                uints: vec![u64::MAX],
                ..Default::default()
            },
        },
        SyncAttribute {
            name: "asset".into(),
            type_name: "asset".into(),
            value: SyncAttributeValue {
                strings: vec!["./textures/image.png".into()],
                ..Default::default()
            },
        },
    ];
    stage.apply_sync_attributes("/World", &attributes).unwrap();
    for attribute in &attributes {
        assert_eq!(
            stage
                .read_sync_attribute("/World", &attribute.name)
                .unwrap()
                .as_ref(),
            Some(attribute)
        );
    }
}

#[test]
fn applying_defaults_preserves_metadata_connections_samples_and_exact_snapshots() {
    let (_fixture, mut stage) = Fixture::open(
        r#"#usda 1.0
def Xform "World" {
    custom float value = 1 (documentation = "keep this")
    float value.timeSamples = { 1: 7, 2: 9 }
    float value.connect = </Other.value>
    custom token blocked = None
    custom float sampled.timeSamples = { 1: 2 }
}
"#,
    );
    let snapshots = stage.root_attribute_snapshots().unwrap();
    let original = snapshots
        .iter()
        .find(|snapshot| snapshot.name == "value")
        .unwrap();
    let non_sync = stage.root_non_sync_text().unwrap();
    assert!(
        stage
            .read_sync_attribute("/World", "blocked")
            .unwrap()
            .is_none()
    );
    assert!(
        stage
            .read_sync_attribute("/World", "sampled")
            .unwrap()
            .is_none()
    );
    stage
        .apply_sync_attributes("/World", &[doubles("value", "float", &[3.0], false)])
        .unwrap();
    assert_eq!(stage.root_non_sync_text().unwrap(), non_sync);
    let changed = stage.root_attribute_snapshots().unwrap();
    let changed = &changed
        .iter()
        .find(|snapshot| snapshot.name == "value")
        .unwrap()
        .text;
    assert!(changed.contains("keep this"));
    assert!(changed.contains("timeSamples"));
    assert!(changed.contains("connect"));
    stage
        .replace_root_attribute_text("/World", "value", &original.text)
        .unwrap();
    assert_eq!(
        stage
            .read_sync_attribute("/World", "value")
            .unwrap()
            .unwrap()
            .value
            .doubles,
        vec![1.0]
    );
}

#[test]
fn invalid_batches_and_reserved_transform_properties_do_not_partially_author() {
    let (_fixture, mut stage) = Fixture::open("#usda 1.0\ndef Xform \"World\" {}\n");
    let before = stage.root_layer_text().unwrap();
    for invalid in [
        doubles("bad", "float3", &[1.0, 2.0], false),
        doubles("bad", "half", &[65505.0], false),
        doubles("bad", "float[]", &[1.0], true),
        doubles("xformOp:translate", "double3", &[1.0, 2.0, 3.0], false),
        SyncAttribute {
            name: "bad".into(),
            type_name: "uint".into(),
            value: SyncAttributeValue {
                uints: vec![u64::MAX],
                ..Default::default()
            },
        },
    ] {
        assert!(
            stage
                .apply_sync_attributes(
                    "/World",
                    &[doubles("valid", "float", &[1.0], false), invalid]
                )
                .is_err()
        );
        assert_eq!(stage.root_layer_text().unwrap(), before);
    }
}

#[test]
fn snapshot_enumeration_and_restoration_include_inactive_and_metadata_only_properties() {
    let (_fixture, mut stage) = Fixture::open(
        r#"#usda 1.0
def Xform "World" (active = false) {
    custom float value (documentation = "metadata only")
}
"#,
    );
    let snapshots = stage.root_attribute_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    stage
        .replace_root_attribute_text("/World", "value", "")
        .unwrap();
    assert!(stage.root_attribute_snapshots().unwrap().is_empty());
    stage
        .replace_root_attribute_text("/World", "value", &snapshots[0].text)
        .unwrap();
    assert_eq!(stage.root_attribute_snapshots().unwrap(), snapshots);
    assert!(
        stage
            .read_sync_attribute("/World", "value")
            .unwrap()
            .is_none()
    );
}

#[test]
fn reads_preserve_root_defaults_when_a_stronger_session_opinion_masks_them() {
    let (fixture, _stage) = Fixture::open(
        r#"#usda 1.0
def Xform "World" {
    custom float value = 1
}
"#,
    );
    let masked = StageService::open_with_session(
        &fixture.0,
        r#"#usda 1.0
over "World" {
    custom float value = 99
}
"#,
        false,
    )
    .unwrap();
    assert_eq!(
        masked
            .read_sync_attribute("/World", "value")
            .unwrap()
            .unwrap()
            .value
            .doubles,
        vec![1.0]
    );
}

#[test]
fn outgoing_assets_are_portable_relative_to_the_scene_root() {
    let (fixture, mut stage) = Fixture::open("#usda 1.0\ndef Xform \"World\" {}\n");
    let path = fixture.0.parent().unwrap().join("textures/image.png");
    stage
        .apply_sync_attributes(
            "/World",
            &[SyncAttribute {
                name: "image".into(),
                type_name: "asset".into(),
                value: SyncAttributeValue {
                    strings: vec![path.to_string_lossy().into_owned()],
                    ..Default::default()
                },
            }],
        )
        .unwrap();
    assert_eq!(
        stage
            .read_sync_attribute("/World", "image")
            .unwrap()
            .unwrap()
            .value
            .strings,
        vec!["./textures/image.png"]
    );
}

#[test]
fn metadata_delta_preserves_a_newer_default_in_both_directions() {
    let (_fixture, mut stage) = Fixture::open(
        r#"#usda 1.0
def Xform "World" {
    custom double value = 1 (documentation = "old")
}
"#,
    );
    let before = stage.root_attribute_text("/World", "value").unwrap();
    let after = before
        .replace("value = 1", "value = 2")
        .replace("old", "new");
    assert_ne!(before, after);
    let (expected, desired) = stage
        .root_attribute_metadata_delta("/World", "value", &before, &after)
        .unwrap();
    stage
        .apply_sync_attributes("/World", &[doubles("value", "double", &[3.0], false)])
        .unwrap();
    stage.apply_root_layer_delta(&expected, &desired).unwrap();
    assert!(
        stage
            .root_attribute_text("/World", "value")
            .unwrap()
            .contains("new")
    );
    assert_eq!(
        stage
            .read_sync_attribute("/World", "value")
            .unwrap()
            .unwrap()
            .value
            .doubles,
        [3.0]
    );
    stage.apply_root_layer_delta(&desired, &expected).unwrap();
    assert!(
        stage
            .root_attribute_text("/World", "value")
            .unwrap()
            .contains("old")
    );
    assert_eq!(
        stage
            .read_sync_attribute("/World", "value")
            .unwrap()
            .unwrap()
            .value
            .doubles,
        [3.0]
    );
    let changed_default = before.replace("value = 1", "value = 99");
    let (expected, desired) = stage
        .root_attribute_metadata_delta("/World", "value", &before, &changed_default)
        .unwrap();
    assert_eq!(
        expected, desired,
        "a default-only change carries no metadata delta"
    );
    let (expected, desired) = stage
        .root_attribute_metadata_delta("/World", "value", "", &after)
        .unwrap();
    stage
        .replace_root_attribute_text("/World", "value", &expected)
        .unwrap();
    stage.apply_root_layer_delta(&expected, &desired).unwrap();
    assert!(
        stage
            .root_attribute_text("/World", "value")
            .unwrap()
            .contains("new")
    );
    assert!(
        stage
            .read_sync_attribute("/World", "value")
            .unwrap()
            .is_none(),
        "a rejected first default must not survive metadata retention"
    );
}
