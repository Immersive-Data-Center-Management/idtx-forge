//! Guest credential writes obey a host-supplied origin group independently of
//! network reachability, under both native and Pulley engines.

use std::collections::BTreeMap;
use bevy_ecs::world::World;
use idtx_behavior::{BehaviorHost, BehaviorManifest, Capabilities, InputRouter, Limits, PlayState, ecs, entity_to_wire};

fn module(selector: Option<&str>) -> Vec<u8> {
    let selector = selector.unwrap_or("");
    let call = if selector.is_empty() { "(call $set (i32.const 128) (i32.const 5))".into() }
        else { format!("(call $scoped (i32.const 256) (i32.const {}) (i32.const 128) (i32.const 5))", selector.len()) };
    wat::parse_str(format!(r#"(module
      (import "idtx" "idtx_auth_set_token" (func $set (param i32 i32) (result i32)))
      (import "idtx" "idtx_auth_set_token_for_host" (func $scoped (param i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
      (import "idtx" "idtx_set_xform" (func $put (param i64 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 128) "token")
      (data (i32.const 256) "{selector}")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_update") (param f32)
        (drop (call $get (call $self) (i32.const 0)))
        (f32.store (i32.const 0) (f32.convert_i32_u {call}))
        (drop (call $put (call $self) (i32.const 0)))))"#)).unwrap()
}

#[test]
fn writer_scopes_are_independent_and_cannot_be_expanded_by_the_guest() {
    idtx_auth::clear();
    let allowed = idtx_auth::AuthScopes::parse(&["https://login.example".into(), "https://*.assets.example:8443".into()]).unwrap();
    for (selector, authorized, expected_code) in [
        (None, false, 4.0_f32),
        (None, true, 0.0),
        (Some("login.example"), true, 0.0),
        (Some("https://cdn.assets.example:8443"), true, 0.0),
        (Some("https://unrelated.example"), true, 4.0),
        (Some("http://login.example"), true, 4.0),
        (Some("https://login.example:8443"), true, 4.0),
    ] {
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        host.set_network_policy(idtx_net_guard::NetworkPolicy::new(true, vec!["unrelated.example".into()]));
        host.register_module(&module(selector), BehaviorManifest {
            name: "auth scope".into(), version: "1".into(), uuid: "auth".into(),
            kind: idtx_behavior_abi::BehaviorKind::Runtime, ship: true, params: BTreeMap::new(),
            inputs: Vec::new(), capabilities: Capabilities::all(), memory_mb: None,
        }).unwrap();
        let mut world = World::new();
        let entity = world.spawn((ecs::Transform::default(), ecs::GlobalTransform::default())).id();
        host.attach(&mut world, entity_to_wire(entity), "auth").unwrap();
        // Updating policy after attachment must also update the store's writer.
        if authorized { host.set_auth_scopes(allowed.clone()); }
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        assert!((world.get::<ecs::Transform>(entity).unwrap().translation.x - expected_code).abs() < f32::EPSILON);
    }
    assert_eq!(idtx_auth::authorization_header_for("https://login.example/data").as_deref(), Some("Bearer token"));
    assert_eq!(idtx_auth::authorization_header_for("https://cdn.assets.example:8443/asset").as_deref(), Some("Bearer token"));
    assert_eq!(idtx_auth::authorization_header_for("https://unrelated.example/data"), None);
    idtx_auth::clear();
    assert_eq!(idtx_auth::authorization_header_for("https://login.example/data"), None);
}
