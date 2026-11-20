//
// Copyright 2016 Pixar
//
// Licensed under the terms set forth in the LICENSE.txt file available at
// https://openusd.org/license.
//
#include "./computeValueSink.h"
#include "pxr/usd/usd/schemaRegistry.h"
#include "pxr/usd/usd/typed.h"

#include "pxr/usd/sdf/types.h"
#include "pxr/usd/sdf/assetPath.h"

PXR_NAMESPACE_OPEN_SCOPE

// Register the schema with the TfType system.
TF_REGISTRY_FUNCTION(TfType)
{
    TfType::Define<IDTXComputeValueSink,
        TfType::Bases< UsdTyped > >();
    
    // Register the usd prim typename as an alias under UsdSchemaBase. This
    // enables one to call
    // TfType::Find<UsdSchemaBase>().FindDerivedByName("ComputeValueSink")
    // to find TfType<IDTXComputeValueSink>, which is how IsA queries are
    // answered.
    TfType::AddAlias<UsdSchemaBase, IDTXComputeValueSink>("ComputeValueSink");
}

/* virtual */
IDTXComputeValueSink::~IDTXComputeValueSink()
{
}

/* static */
IDTXComputeValueSink
IDTXComputeValueSink::Get(const UsdStagePtr &stage, const SdfPath &path)
{
    if (!stage) {
        TF_CODING_ERROR("Invalid stage");
        return IDTXComputeValueSink();
    }
    return IDTXComputeValueSink(stage->GetPrimAtPath(path));
}

/* static */
IDTXComputeValueSink
IDTXComputeValueSink::Define(
    const UsdStagePtr &stage, const SdfPath &path)
{
    static TfToken usdPrimTypeName("ComputeValueSink");
    if (!stage) {
        TF_CODING_ERROR("Invalid stage");
        return IDTXComputeValueSink();
    }
    return IDTXComputeValueSink(
        stage->DefinePrim(path, usdPrimTypeName));
}

/* virtual */
UsdSchemaKind IDTXComputeValueSink::_GetSchemaKind() const
{
    return IDTXComputeValueSink::schemaKind;
}

/* static */
const TfType &
IDTXComputeValueSink::_GetStaticTfType()
{
    static TfType tfType = TfType::Find<IDTXComputeValueSink>();
    return tfType;
}

/* static */
bool 
IDTXComputeValueSink::_IsTypedSchema()
{
    static bool isTyped = _GetStaticTfType().IsA<UsdTyped>();
    return isTyped;
}

/* virtual */
const TfType &
IDTXComputeValueSink::_GetTfType() const
{
    return _GetStaticTfType();
}

UsdAttribute
IDTXComputeValueSink::GetDouble_valueAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->double_value);
}

UsdAttribute
IDTXComputeValueSink::CreateDouble_valueAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->double_value,
                       SdfValueTypeNames->Double,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXComputeValueSink::GetFloat_valueAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->float_value);
}

UsdAttribute
IDTXComputeValueSink::CreateFloat_valueAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->float_value,
                       SdfValueTypeNames->Float,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

UsdAttribute
IDTXComputeValueSink::GetString_valueAttr() const
{
    return GetPrim().GetAttribute(IDTXTokens->string_value);
}

UsdAttribute
IDTXComputeValueSink::CreateString_valueAttr(VtValue const &defaultValue, bool writeSparsely) const
{
    return UsdSchemaBase::_CreateAttr(IDTXTokens->string_value,
                       SdfValueTypeNames->String,
                       /* custom = */ false,
                       SdfVariabilityVarying,
                       defaultValue,
                       writeSparsely);
}

namespace {
static inline TfTokenVector
_ConcatenateAttributeNames(const TfTokenVector& left,const TfTokenVector& right)
{
    TfTokenVector result;
    result.reserve(left.size() + right.size());
    result.insert(result.end(), left.begin(), left.end());
    result.insert(result.end(), right.begin(), right.end());
    return result;
}
}

/*static*/
const TfTokenVector&
IDTXComputeValueSink::GetSchemaAttributeNames(bool includeInherited)
{
    static TfTokenVector localNames = {
        IDTXTokens->double_value,
        IDTXTokens->float_value,
        IDTXTokens->string_value,
    };
    static TfTokenVector allNames =
        _ConcatenateAttributeNames(
            UsdTyped::GetSchemaAttributeNames(true),
            localNames);

    if (includeInherited)
        return allNames;
    else
        return localNames;
}

PXR_NAMESPACE_CLOSE_SCOPE

// ===================================================================== //
// Feel free to add custom code below this line. It will be preserved by
// the code generator.
//
// Just remember to wrap code in the appropriate delimiters:
// 'PXR_NAMESPACE_OPEN_SCOPE', 'PXR_NAMESPACE_CLOSE_SCOPE'.
// ===================================================================== //
// --(BEGIN CUSTOM CODE)--
