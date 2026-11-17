//! Validation of generic deferred reference requests, independent of USD.
use crate::session_edit::AttributeConnection;
use idtx_behavior_abi::{
    ErrorCode, MAX_BEHAVIOR_CONTEXT_BYTES, MAX_SPAWN_CONNECTIONS, MAX_SPAWN_PAYLOAD_BYTES, Xform,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    context: serde_json::Value,
    #[serde(default)]
    connections: Vec<AttributeConnection>,
}

pub fn valid_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn valid_property(property: &str) -> bool {
    !property.is_empty() && property.split(':').all(valid_identifier)
}

pub fn validate_connection(connection: &AttributeConnection) -> Result<(), ErrorCode> {
    if connection.child.is_empty()
        || connection.child.len() > 2048
        || connection.property.len() > 256
        || connection.source.len() > 4096
        || !connection.child.split('/').all(valid_identifier)
        || !valid_property(&connection.property)
    {
        return Err(ErrorCode::InvalidArgument);
    }
    let Some((prim, property)) = connection.source.rsplit_once('.') else {
        return Err(ErrorCode::InvalidArgument);
    };
    // Relative paths are resolved against the selected parent by the session.
    // Every segment must be an identifier, excluding traversal and variant syntax.
    let prim = prim.strip_prefix('/').unwrap_or(prim);
    if !prim.split('/').all(valid_identifier) || !valid_property(property) {
        return Err(ErrorCode::InvalidArgument);
    }
    Ok(())
}

pub fn validate_reference(
    name: &str,
    prototype: &str,
    transform: &Xform,
    context_json: &str,
    connections: &[AttributeConnection],
) -> Result<(), ErrorCode> {
    if name.len() > 256
        || !valid_identifier(name)
        || prototype.is_empty()
        || prototype.len() > 256
        || !prototype
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        || context_json.len() > MAX_BEHAVIOR_CONTEXT_BYTES
        || connections.len() > MAX_SPAWN_CONNECTIONS
    {
        return Err(ErrorCode::InvalidArgument);
    }
    let t = glam::Vec3::from(transform.translation);
    let r = glam::Quat::from(transform.rotation);
    let s = glam::Vec3::from(transform.scale);
    if !t.is_finite()
        || !r.is_finite()
        || !s.is_finite()
        || (r.length_squared() - 1.0).abs() > 0.001
        || s.to_array().iter().any(|v| v.abs() < 0.000001)
    {
        return Err(ErrorCode::InvalidArgument);
    }
    serde_json::from_str::<serde_json::Value>(context_json).map_err(|_| ErrorCode::TypeMismatch)?;
    for connection in connections {
        validate_connection(connection)?;
    }
    Ok(())
}

pub(crate) fn decode_payload(
    payload: &str,
) -> Result<(String, Vec<AttributeConnection>), ErrorCode> {
    if payload.len() > MAX_SPAWN_PAYLOAD_BYTES {
        return Err(ErrorCode::InvalidArgument);
    }
    let decoded: Payload = serde_json::from_str(payload).map_err(|_| ErrorCode::TypeMismatch)?;
    let context = serde_json::to_string(&decoded.context).map_err(|_| ErrorCode::TypeMismatch)?;
    if context.len() > MAX_BEHAVIOR_CONTEXT_BYTES
        || decoded.connections.len() > MAX_SPAWN_CONNECTIONS
    {
        return Err(ErrorCode::InvalidArgument);
    }
    Ok((context, decoded.connections))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xform() -> Xform {
        Xform {
            translation: glam::Vec3::ZERO.into(),
            rotation: glam::Quat::IDENTITY.into(),
            scale: glam::Vec3::ONE.into(),
            ..Default::default()
        }
    }

    #[test]
    fn reference_rejects_malformed_context_identifiers_and_nonfinite_transforms() {
        assert_eq!(
            validate_reference("Panel", "overview", &xform(), "{}", &[]),
            Ok(())
        );
        for name in ["", "../Panel", "Panel/Child", "Panel{variant=x}", "1Panel"] {
            assert_eq!(
                validate_reference(name, "overview", &xform(), "{}", &[]),
                Err(ErrorCode::InvalidArgument)
            );
        }
        for prototype in ["", "../overview", "assets/template.usda", "overview:bad"] {
            assert_eq!(
                validate_reference("Panel", prototype, &xform(), "{}", &[]),
                Err(ErrorCode::InvalidArgument)
            );
        }
        assert_eq!(
            validate_reference("Panel", "overview", &xform(), "{bad", &[]),
            Err(ErrorCode::TypeMismatch)
        );
        assert_eq!(
            validate_reference(
                "Panel",
                "overview",
                &xform(),
                &" ".repeat(MAX_BEHAVIOR_CONTEXT_BYTES + 1),
                &[]
            ),
            Err(ErrorCode::InvalidArgument)
        );
        let mut invalid = xform();
        invalid.translation.x = f32::NAN;
        assert_eq!(
            validate_reference("Panel", "overview", &invalid, "{}", &[]),
            Err(ErrorCode::InvalidArgument)
        );
        invalid = xform();
        invalid.rotation.w = 0.0;
        assert_eq!(
            validate_reference("Panel", "overview", &invalid, "{}", &[]),
            Err(ErrorCode::InvalidArgument)
        );
        invalid = xform();
        invalid.scale.x = 0.0;
        assert_eq!(
            validate_reference("Panel", "overview", &invalid, "{}", &[]),
            Err(ErrorCode::InvalidArgument)
        );
    }

    #[test]
    fn connections_accept_parent_relative_paths_and_reject_subtree_escape() {
        let mut connection = AttributeConnection {
            child: "Row/Value".into(),
            property: "idtx:widget:value".into(),
            source: "Compute/Temperature.outputs:result".into(),
        };
        assert_eq!(validate_connection(&connection), Ok(()));
        connection.source = "/World/Asset/Compute/Temperature.outputs:result".into();
        assert_eq!(validate_connection(&connection), Ok(()));
        for source in [
            "../Other.outputs:result",
            "Compute/../Other.outputs:result",
            "//World/Other.outputs:result",
            "Compute.outputs:",
            "Compute.outputs:bad-name",
            "Compute{v=x}.outputs:result",
            "Compute\\Other.outputs:result",
        ] {
            connection.source = source.into();
            assert_eq!(
                validate_connection(&connection),
                Err(ErrorCode::InvalidArgument),
                "{source}"
            );
        }
        connection.source = "Compute/Temperature.outputs:result".into();
        for child in ["", "../Value", "/Other/Value", "Row//Value", "Row/."] {
            connection.child = child.into();
            assert_eq!(
                validate_connection(&connection),
                Err(ErrorCode::InvalidArgument),
                "{child}"
            );
        }
    }

    #[test]
    fn payload_requires_context_and_bounds_connections_before_queueing() {
        assert!(matches!(decode_payload("{}"), Err(ErrorCode::TypeMismatch)));
        assert!(matches!(
            decode_payload(r#"{"context":{},"extra":true}"#),
            Err(ErrorCode::TypeMismatch)
        ));
        assert!(matches!(
            decode_payload(&" ".repeat(MAX_SPAWN_PAYLOAD_BYTES + 1)),
            Err(ErrorCode::InvalidArgument)
        ));
        let connection = serde_json::json!({"child":"Value", "property":"idtx:widget:value", "source":"Compute.outputs:result"});
        let payload = serde_json::json!({"context":{}, "connections":vec![connection; MAX_SPAWN_CONNECTIONS + 1]}).to_string();
        assert!(matches!(
            decode_payload(&payload),
            Err(ErrorCode::InvalidArgument)
        ));
    }
}
