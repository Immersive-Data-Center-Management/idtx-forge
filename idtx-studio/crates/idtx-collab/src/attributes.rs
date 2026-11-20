//! Validation of Core's typed attribute-default protocol.
use std::collections::HashSet;

use prost::Message as _;

use crate::proto::{AttributeValue, PrimAttribute};

const MAX_ATTRIBUTES: usize = 1024;
const MAX_COMPONENTS: usize = 4 * 1024 * 1024;
const MAX_STRING_BYTES: usize = 4 * 1024 * 1024;
const MAX_VALUE_BYTES: usize = 8 * 1024 * 1024;

/// Transform properties must continue through the transform protocol.
#[must_use]
pub fn is_transform_attribute(name: &str) -> bool {
    name == "xformOpOrder" || name.starts_with("xformOp:")
}

/// Validate a bounded batch without inferring the USD type from its value.
pub fn validate_attributes(attributes: &[PrimAttribute]) -> Result<(), String> {
    if attributes.is_empty() || attributes.len() > MAX_ATTRIBUTES {
        return Err("attribute batch must contain between 1 and 1024 entries".into());
    }
    let mut names = HashSet::new();
    let mut components = 0usize;
    let mut string_bytes = 0usize;
    let mut encoded_bytes = 0usize;
    for attribute in attributes {
        if !valid_property_name(&attribute.name) || !names.insert(&attribute.name) {
            return Err(format!(
                "invalid or duplicate attribute name: {}",
                attribute.name
            ));
        }
        if is_transform_attribute(&attribute.name) {
            return Err(format!("{} requires a transform update", attribute.name));
        }
        let value = attribute
            .value
            .as_ref()
            .ok_or_else(|| format!("{} has no value", attribute.name))?;
        validate_value(&attribute.type_name, value)?;
        components = components
            .saturating_add(value.doubles.len())
            .saturating_add(value.ints.len())
            .saturating_add(value.uints.len())
            .saturating_add(value.bools.len())
            .saturating_add(value.strings.len());
        string_bytes = string_bytes
            .saturating_add(attribute.name.len())
            .saturating_add(attribute.type_name.len());
        for string in &value.strings {
            string_bytes = string_bytes.saturating_add(string.len());
        }
        encoded_bytes = encoded_bytes.saturating_add(attribute.encoded_len());
        if components > MAX_COMPONENTS
            || string_bytes > MAX_STRING_BYTES
            || encoded_bytes > MAX_VALUE_BYTES
        {
            return Err("attribute batch exceeds value limits".into());
        }
    }
    Ok(())
}

fn valid_property_name(name: &str) -> bool {
    name.split(':').all(|part| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|c| c == '_' || unicode_ident::is_xid_start(c))
            && chars.all(unicode_ident::is_xid_continue)
    })
}

/// Validate scalar/array component lanes and the server's narrowing policy.
pub fn validate_value(type_name: &str, value: &AttributeValue) -> Result<(), String> {
    let (lane, width, max_float) = type_layout(type_name)
        .ok_or_else(|| format!("unsupported scalar USD attribute type: {type_name}"))?;
    let lengths = [
        value.doubles.len(),
        value.ints.len(),
        value.uints.len(),
        value.bools.len(),
        value.strings.len(),
    ];
    if lengths
        .iter()
        .enumerate()
        .any(|(i, length)| i != lane && *length != 0)
    {
        return Err(format!("{type_name} has values in an unused lane"));
    }
    let count = lengths[lane];
    if (value.is_array && !count.is_multiple_of(width)) || (!value.is_array && count != width) {
        return Err(format!("{type_name} has an invalid component count"));
    }
    if value
        .doubles
        .iter()
        .any(|v| v.is_finite() && v.abs() > max_float)
    {
        return Err(format!("{type_name} component overflows its USD type"));
    }
    if matches!(type_name, "int" | "int2" | "int3" | "int4")
        && value.ints.iter().any(|v| i32::try_from(*v).is_err())
    {
        return Err(format!("{type_name} component overflows int32"));
    }
    let max_uint = match type_name {
        "uchar" => u64::from(u8::MAX),
        "uint" => u64::from(u32::MAX),
        _ => u64::MAX,
    };
    if value.uints.iter().any(|v| *v > max_uint) {
        return Err(format!("{type_name} component overflows its unsigned type"));
    }
    Ok(())
}

// (lane, components per element, maximum finite floating component). Roles
// share a concrete value family but keep their explicit scalar token on wire.
fn type_layout(name: &str) -> Option<(usize, usize, f64)> {
    let float_max = f64::from(f32::MAX);
    Some(match name {
        "bool" => (3, 1, f64::MAX),
        "string" | "token" | "asset" => (4, 1, f64::MAX),
        "uchar" | "uint" | "uint64" => (2, 1, f64::MAX),
        "int" | "int64" => (1, 1, f64::MAX),
        "int2" => (1, 2, f64::MAX),
        "int3" => (1, 3, f64::MAX),
        "int4" => (1, 4, f64::MAX),
        "half" => (0, 1, 65504.0),
        "float" => (0, 1, float_max),
        "double" => (0, 1, f64::MAX),
        "half2" | "texCoord2h" => (0, 2, 65504.0),
        "float2" | "texCoord2f" => (0, 2, float_max),
        "double2" | "texCoord2d" => (0, 2, f64::MAX),
        "half3" | "color3h" | "point3h" | "normal3h" | "vector3h" | "texCoord3h" => (0, 3, 65504.0),
        "float3" | "color3f" | "point3f" | "normal3f" | "vector3f" | "texCoord3f" => {
            (0, 3, float_max)
        }
        "double3" | "color3d" | "point3d" | "normal3d" | "vector3d" | "texCoord3d" => {
            (0, 3, f64::MAX)
        }
        "half4" | "color4h" => (0, 4, 65504.0),
        "float4" | "color4f" | "quatf" => (0, 4, float_max),
        "double4" | "color4d" | "quatd" => (0, 4, f64::MAX),
        "matrix4d" | "frame4d" => (0, 16, f64::MAX),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{
        AttributeError, AttributeFailureReason, BaseMessage, SetPrimAttributesAck,
        SetPrimAttributesBroadcast, SetPrimAttributesRequest, base_message,
    };

    fn color(doubles: Vec<f64>, is_array: bool) -> PrimAttribute {
        PrimAttribute {
            name: "primvars:displayColor".into(),
            type_name: "color3f".into(),
            value: Some(AttributeValue {
                doubles,
                is_array,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn role_and_empty_or_single_element_arrays_survive_wire_roundtrip() {
        for attribute in [
            color(vec![], true),
            color(vec![0.1, 0.2, 0.3], true),
            color(vec![0.1, 0.2, 0.3], false),
        ] {
            validate_attributes(std::slice::from_ref(&attribute)).unwrap();
            let message = BaseMessage {
                payload: Some(base_message::Payload::SetPrimAttributes(
                    SetPrimAttributesRequest {
                        session_id: "test".into(),
                        prim_path: "/World".into(),
                        attributes: vec![attribute],
                    },
                )),
                request_id: 1,
                ..Default::default()
            };
            assert_eq!(
                BaseMessage::decode(message.encode_to_vec().as_slice()).unwrap(),
                message
            );
            assert_eq!(
                message.encode_to_vec()[0],
                0x82,
                "request uses Core oneof field 16"
            );
        }
    }

    #[test]
    fn rejects_absent_values_wrong_lanes_widths_and_array_tokens() {
        let mut attribute = color(vec![1.0; 3], false);
        attribute.value = None;
        assert!(validate_attributes(&[attribute]).is_err());
        assert!(validate_attributes(&[color(vec![1.0; 2], false)]).is_err());
        assert!(validate_attributes(&[color(vec![1.0; 4], true)]).is_err());
        let mut attribute = color(vec![1.0; 3], true);
        attribute.value.as_mut().unwrap().ints.push(1);
        assert!(validate_attributes(&[attribute]).is_err());
        let mut attribute = color(vec![], true);
        attribute.type_name.push_str("[]");
        assert!(validate_attributes(&[attribute]).is_err());
    }

    #[test]
    fn unsigned_values_keep_full_range_and_narrow_values_are_checked() {
        let value = AttributeValue {
            uints: vec![u64::MAX],
            ..Default::default()
        };
        validate_value("uint64", &value).unwrap();
        assert!(validate_value("uint", &value).is_err());
        let value = AttributeValue {
            ints: vec![i64::MIN],
            ..Default::default()
        };
        validate_value("int64", &value).unwrap();
        assert!(validate_value("int", &value).is_err());
        let value = AttributeValue {
            doubles: vec![65505.0],
            ..Default::default()
        };
        assert!(validate_value("half", &value).is_err());
        let value = AttributeValue {
            doubles: vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY],
            is_array: true,
            ..Default::default()
        };
        validate_value("float", &value).unwrap();
    }

    #[test]
    fn broadcast_and_partial_ack_use_core_wire_tags() {
        let broadcast = BaseMessage {
            payload: Some(base_message::Payload::SetPrimAttributesBroadcast(
                SetPrimAttributesBroadcast {
                    prim_path: "/World".into(),
                    attributes: vec![color(vec![], true)],
                },
            )),
            server_seq: 12,
            ..Default::default()
        };
        let ack = BaseMessage {
            payload: Some(base_message::Payload::SetPrimAttributesAck(
                SetPrimAttributesAck {
                    prim_path: "/World".into(),
                    failed: vec![AttributeError {
                        name: "customAttr".into(),
                        reason: AttributeFailureReason::TypeMismatch as i32,
                        detail: "composed type is token".into(),
                    }],
                },
            )),
            request_id: 42,
            server_seq: 12,
            ..Default::default()
        };
        for (message, tag) in [(broadcast, 0x8a), (ack, 0x92)] {
            let bytes = message.encode_to_vec();
            assert_eq!(bytes[0], tag);
            assert_eq!(bytes[1], 1);
            assert_eq!(BaseMessage::decode(bytes.as_slice()).unwrap(), message);
        }
        let value = AttributeValue {
            ints: vec![-1],
            ..Default::default()
        };
        assert_eq!(
            value.encode_to_vec(),
            vec![0x12, 1, 1],
            "signed integers use Core's sint64 zigzag lane"
        );
    }

    #[test]
    fn batches_reject_duplicates_and_transform_properties() {
        let attribute = color(vec![], true);
        assert!(validate_attributes(&[attribute.clone(), attribute]).is_err());
        for name in ["xformOpOrder", "xformOp:translate", "bad::name"] {
            let mut attribute = color(vec![], true);
            attribute.name = name.into();
            assert!(validate_attributes(&[attribute]).is_err());
        }
        assert!(validate_attributes(&[]).is_err());
        let mut attribute = color(vec![], true);
        attribute.name = "属性:éclairage".into();
        validate_attributes(&[attribute]).unwrap();
    }
}
